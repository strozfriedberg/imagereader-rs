use std::io;
use std::sync::Arc;

use s3::bucket::Bucket;
use s3::error::S3Error;
use s3::serde_types::HeadObjectResult;
use tokio::runtime::Runtime;
use tracing::warn;
use url::Url;

use crate::{
    errors::{OpenError, OpenErrorKind},
    s3_creds::{S3Auth, credentials_rotated, snapshot_credentials_sync},
    urlsource::{s3_bucket, s3_key},
};

/// We could not tell whether a segment is there.
///
/// Distinct from `Ok(false)` on purpose. Segment discovery walks upward until a
/// name is absent, so "absent" is meaningful: it terminates the sequence. A
/// throttled HEAD, an expired credential, or an unreadable directory reported
/// as absent silently truncates the image -- the reader opens, the partition
/// table parses, and the short read only shows up much later as corruption.
/// Anything short of a definite answer has to stop the open instead, with one
/// documented exception: S3's 403, which is genuinely ambiguous and is resolved
/// by policy in [`classify_head_status`].
#[derive(Debug, thiserror::Error)]
#[error("could not determine whether {path} exists: {source}")]
pub struct ExistsError {
    pub path: String,
    #[source]
    pub source: io::Error,
}

impl ExistsError {
    pub fn new<P: AsRef<str>>(path: P, source: io::Error) -> Self {
        Self {
            path: path.as_ref().to_string(),
            source,
        }
    }
}

/// Decides whether a candidate segment path exists, so segment discovery can
/// stop at the end of the sequence. Separate from the naming convention, which
/// differs per format: e01 has a defined extension scheme, split raw does not.
///
/// `Ok(false)` means absent, and it ends the sequence, so implementations must
/// not return it for a probe they could not actually answer. Everything else is
/// an `Err`; see [`ExistsError`] for why the distinction matters.
pub trait ExistsChecker {
    fn exists<T: AsRef<str>>(&mut self, path: T) -> Result<bool, ExistsError>;
}

pub struct FileChecker;

impl ExistsChecker for FileChecker {
    fn exists<T: AsRef<str>>(&mut self, path: T) -> Result<bool, ExistsError> {
        let path = path.as_ref();
        match std::fs::metadata(path) {
            Ok(md) => Ok(md.is_file()),
            // NotFound is the ordinary end of a sequence. NotADirectory means a
            // path component is a file, so nothing can exist below it -- also a
            // definite no. A permission error is not: it says we are not allowed
            // to look, which is exactly the case `Path::is_file()` used to report
            // as "not there".
            Err(e)
                if matches!(
                    e.kind(),
                    io::ErrorKind::NotFound | io::ErrorKind::NotADirectory
                ) =>
            {
                Ok(false)
            }
            Err(e) => Err(ExistsError::new(path, e)),
        }
    }
}

pub struct S3Checker {
    /// Built once and kept. Discovery probes one name per segment, so rebuilding
    /// this per call meant a credential snapshot -- two `block_on` hops, an async
    /// mutex and a `Credentials` clone -- plus a fresh `Bucket` for every probe,
    /// several hundred times over a long sequence, all to arrive at the same
    /// bucket each time.
    bucket: Bucket,
    runtime: Arc<Runtime>,
    auth: Arc<S3Auth>,
}

impl S3Checker {
    pub fn new(url: &Url, runtime: Arc<Runtime>, auth: Arc<S3Auth>) -> Result<Self, OpenError> {
        let name = url
            .host_str()
            .ok_or_else(|| OpenError::from(OpenErrorKind::BadPath(url.to_string())))?;
        // `s3_bucket` also settles the bucket's real region, which is why the
        // bucket it returns is worth keeping rather than reducing to a name.
        let bucket = s3_bucket(name, url.as_ref(), &runtime, &auth)?;
        Ok(Self {
            bucket,
            runtime,
            auth,
        })
    }

    /// Bring the retained bucket's credentials up to date, rewriting them only
    /// when they have actually rotated.
    ///
    /// Keeping one bucket must not mean keeping stale credentials: discovery
    /// over a long sequence can outlive a short-lived session token, and a
    /// bucket still signing with the old one would start returning 403 -- which
    /// this checker correctly refuses to read as "absent", so the open would
    /// fail rather than truncate, but fail needlessly.
    fn sync_credentials(&mut self) -> Result<(), io::Error> {
        let fresh = snapshot_credentials_sync(&self.runtime, &self.auth)?;
        let stale = match self.runtime.block_on(self.bucket.credentials()) {
            Ok(current) => credentials_rotated(&current, &fresh),
            // Unreadable credentials are not something to reason about; replace.
            Err(_) => true,
        };
        if stale {
            self.bucket.set_credentials(fresh);
        }
        Ok(())
    }
}

impl ExistsChecker for S3Checker {
    fn exists<T: AsRef<str>>(&mut self, path: T) -> Result<bool, ExistsError> {
        let path = path.as_ref();
        let err = |e: io::Error| ExistsError::new(path, e);

        let url = Url::parse(path).map_err(|e| err(io::Error::other(e)))?;

        // The key must be spelled exactly as `source_for_url` will spell it when
        // it opens the segment. Probing the raw, still-encoded path makes every
        // segment of an image under a prefix with a space look absent, and
        // discovery reads that as "not a split image".
        let key =
            s3_key(&url).ok_or_else(|| err(io::Error::other("object key is not valid UTF-8")))?;

        self.sync_credentials().map_err(&err)?;

        let outcome = self.runtime.block_on(self.bucket.head_object(&key));
        match head_verdict(outcome).map_err(|e| err(io::Error::other(e)))? {
            HeadVerdict::Present => Ok(true),
            HeadVerdict::Absent => Ok(false),
            HeadVerdict::AbsentOrForbidden => {
                warn!(
                    "HEAD {path} returned 403; treating the segment as absent. \
                     Expected if this bucket grants GetObject without ListBucket, \
                     but a genuine permission problem here ends the segment \
                     sequence early and opens a short image."
                );
                Ok(false)
            }
            HeadVerdict::Undetermined => unreachable!("head_verdict returns Err instead"),
        }
    }
}

/// Reads a `head_object` outcome as an existence verdict.
///
/// The status does not always arrive as a status. rust-s3 is built with its
/// default `fail-on-err` feature on, so any non-2xx HEAD comes back as
/// `Err(HttpFailWithBody(status, body))`, not as `Ok((_, status))` -- and the
/// 404 that ends every segment sequence is exactly such a status. It has to be
/// unwrapped back out of the error before [`classify_head_status`] can read it,
/// or the end of the sequence looks like a failed probe and the open aborts.
///
/// An `Undetermined` status is returned as an error rather than as a verdict,
/// so the original error keeps its response body for the message.
fn head_verdict(outcome: Result<(HeadObjectResult, u16), S3Error>) -> Result<HeadVerdict, S3Error> {
    let (code, reported) = match outcome {
        Ok((_, code)) => (code, None),
        Err(S3Error::HttpFailWithBody(code, body)) => (code, Some((code, body))),
        // Anything else -- a transport failure, a signing failure -- carries no
        // status to read, and is undetermined by definition.
        Err(e) => return Err(e),
    };

    match classify_head_status(code) {
        HeadVerdict::Undetermined => Err(match reported {
            Some((code, body)) => S3Error::HttpFailWithBody(code, body),
            None => S3Error::HttpFailWithBody(code, String::new()),
        }),
        verdict => Ok(verdict),
    }
}

/// What a HEAD status says about whether the object is there.
#[derive(Debug, PartialEq, Eq)]
enum HeadVerdict {
    Present,
    Absent,
    /// 403, which S3 uses for both "forbidden" and "not found" and which the
    /// caller must therefore resolve by policy rather than by reading it.
    AbsentOrForbidden,
    /// No usable answer; the caller must not treat this as absent.
    Undetermined,
}

/// Reads a HEAD status as an existence verdict.
///
/// 403 is the interesting one. S3 answers a HEAD for a key that does not exist
/// with 403 rather than 404 whenever the principal lacks `s3:ListBucket` -- a
/// very common read-only grant, since GetObject alone is enough to read an
/// image. Discovery needs a definite negative to terminate, so treating 403 as
/// undetermined makes the very probe that ends the sequence fail, and every
/// multi-segment open against such a bucket fails outright.
///
/// So 403 is reported as absent, and the caller is expected to say so out loud.
/// 5xx and throttling stay undetermined; that is where the real truncation risk
/// lives, and unlike 403 they carry no legitimate "this is just the end of the
/// sequence" reading.
fn classify_head_status(code: u16) -> HeadVerdict {
    match code {
        200 => HeadVerdict::Present,
        // 410 is a delete marker: gone, definitively.
        404 | 410 => HeadVerdict::Absent,
        403 => HeadVerdict::AbsentOrForbidden,
        _ => HeadVerdict::Undetermined,
    }
}

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn file_checker_finds_a_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("seg.001");
        std::fs::write(&path, b"x").unwrap();
        assert!(FileChecker.exists(path.to_str().unwrap()).unwrap());
    }

    /// The ordinary end of a sequence: absent, not an error.
    #[test]
    fn file_checker_reports_a_missing_file_as_absent() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("seg.002");
        assert!(!FileChecker.exists(path.to_str().unwrap()).unwrap());
    }

    /// A directory is not a segment, but it is a definite answer.
    #[test]
    fn file_checker_reports_a_directory_as_absent() {
        let dir = tempfile::tempdir().unwrap();
        assert!(!FileChecker.exists(dir.path().to_str().unwrap()).unwrap());
    }

    /// A path component that is a file means nothing can exist below it.
    #[test]
    fn file_checker_reports_a_non_directory_component_as_absent() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("notadir");
        std::fs::write(&file, b"x").unwrap();
        let under = file.join("seg.001");
        assert!(!FileChecker.exists(under.to_str().unwrap()).unwrap());
    }

    /// Only a status that genuinely means "not there" may end a sequence.
    /// Getting 5xx or throttling wrong here truncates an image silently.
    #[test]
    fn head_status_classification() {
        assert_eq!(classify_head_status(200), HeadVerdict::Present);
        assert_eq!(classify_head_status(404), HeadVerdict::Absent);
        assert_eq!(classify_head_status(410), HeadVerdict::Absent);

        // 403 means "not found" on a bucket granting GetObject without
        // ListBucket, so it cannot be an error without breaking those buckets
        // entirely -- but it is not a clean negative either.
        assert_eq!(classify_head_status(403), HeadVerdict::AbsentOrForbidden);

        // Everything else must stop the open rather than end the sequence.
        for code in [429, 500, 502, 503, 504, 301, 400] {
            assert_eq!(
                classify_head_status(code),
                HeadVerdict::Undetermined,
                "HTTP {code} must not be read as the end of a sequence"
            );
        }
    }

    /// The regression this fixed: with rust-s3's `fail-on-err` feature on (it is
    /// on by default, and this crate does not turn it off), the 404 that ends a
    /// segment sequence arrives as an error rather than as a status. Read as a
    /// failed probe, it aborts the open of every single-segment S3 image.
    #[test]
    fn a_404_delivered_as_an_error_is_still_absent() {
        let got = head_verdict(Err(S3Error::HttpFailWithBody(404, String::new())))
            .expect("a 404 ends the sequence; it is not a failed probe");
        assert_eq!(got, HeadVerdict::Absent);
    }

    /// Same delivery path as the 404, and the same reason it cannot be an error:
    /// a bucket granting GetObject without ListBucket answers 403 for a key that
    /// is not there.
    #[test]
    fn a_403_delivered_as_an_error_is_absent_or_forbidden() {
        let got = head_verdict(Err(S3Error::HttpFailWithBody(403, String::new())))
            .expect("403 is resolved by policy, not surfaced as a probe failure");
        assert_eq!(got, HeadVerdict::AbsentOrForbidden);
    }

    #[test]
    fn a_200_is_present() {
        let got = head_verdict(Ok((HeadObjectResult::default(), 200))).unwrap();
        assert_eq!(got, HeadVerdict::Present);
    }

    /// The statuses that must stop the open keep stopping it -- including their
    /// response body, which is often the only clue about what went wrong.
    #[test]
    fn an_undetermined_status_stays_an_error_with_its_body() {
        for code in [429, 500, 503] {
            let err = head_verdict(Err(S3Error::HttpFailWithBody(code, "slow down".into())))
                .expect_err("HTTP {code} must not be read as the end of a sequence");
            let msg = err.to_string();
            assert!(msg.contains(&code.to_string()), "{msg}");
            assert!(msg.contains("slow down"), "{msg}");
        }
    }

    /// An error with no status in it -- a connection reset, a signing failure --
    /// has nothing to classify and must not become "absent".
    #[test]
    fn a_statusless_error_is_not_absent() {
        let err = head_verdict(Err(S3Error::HttpFail)).expect_err("no status means no verdict");
        assert!(matches!(err, S3Error::HttpFail));
    }

    /// The regression: an unreadable directory used to report every candidate as
    /// absent, which discovery reads as the end of the sequence. It has to be an
    /// error instead, or the image opens short.
    #[cfg(unix)]
    #[test]
    fn file_checker_reports_an_unreadable_directory_as_an_error() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        let sub = dir.path().join("locked");
        std::fs::create_dir(&sub).unwrap();
        std::fs::write(sub.join("seg.001"), b"x").unwrap();
        std::fs::set_permissions(&sub, std::fs::Permissions::from_mode(0o000)).unwrap();

        let probe = sub.join("seg.001");
        let root = std::fs::read(&probe).is_ok();
        let got = FileChecker.exists(probe.to_str().unwrap());

        // Restore before asserting, so a failure still lets tempdir clean up.
        std::fs::set_permissions(&sub, std::fs::Permissions::from_mode(0o700)).unwrap();

        if root {
            eprintln!("skipping: permission bits do not apply to this user");
            return;
        }

        let err = got.expect_err("an unreadable directory must not read as absent");
        assert_eq!(err.source.kind(), io::ErrorKind::PermissionDenied);
        assert!(err.to_string().contains("seg.001"), "{err}");
    }
}
