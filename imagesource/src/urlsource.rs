use percent_encoding::percent_decode_str;
use s3::{bucket::Bucket, region::Region};
use std::{path::Path, str::FromStr, sync::Arc};
use tokio::runtime::Runtime;
use tracing::debug;
use url::Url;

use crate::{
    bytessource::BytesSource,
    errors::{OpenError, OpenErrorKind},
    filesource::FileSource,
    io_log::IoLog,
    s3_creds::{S3Auth, s3_region_name, snapshot_credentials_sync},
    s3source::S3Source,
};

pub fn path_or_url_to_url<P: AsRef<str>>(p: P) -> Option<Url> {
    match Url::parse(p.as_ref()) {
        // might be a path; make it absolute and reparse
        Err(url::ParseError::RelativeUrlWithoutBase) => Path::new(p.as_ref())
            .canonicalize()
            .map(Url::from_file_path)
            .map_err(|_| ())
            // FIXME: use flatten after Rust 1.89
            //            .flatten()
            .and_then(|r| r)
            .ok(),
        r => r.ok(),
    }
}

fn s3_region_for_host_in_region(name: &str, region_name: &str) -> Region {
    if name.ends_with("-s3alias") || name.ends_with("-ext-s3alias") {
        Region::Custom {
            region: region_name.to_string(),
            endpoint: format!("s3-accesspoint.{region_name}.amazonaws.com"),
        }
    } else {
        Region::from_str(region_name).unwrap_or(Region::UsEast1)
    }
}

fn s3_region_configured(auth: &S3Auth) -> bool {
    s3_region_name(Some(auth)).is_some()
}

fn s3_region_for_host(name: &str, auth: Option<&S3Auth>) -> Region {
    // `us-east-1` here is only the bootstrap endpoint used to issue the
    // GetBucketLocation discovery call when no region is configured; it is not a
    // regional default. A configured region (env/profile) is used as-is, and an
    // unconfigured bucket's real region is discovered in `s3_bucket`.
    let region_name = s3_region_name(auth).unwrap_or_else(|| "us-east-1".to_string());
    s3_region_for_host_in_region(name, &region_name)
}

pub fn s3_bucket(
    name: &str,
    ctx: &str,
    runtime: &Runtime,
    auth: &Arc<S3Auth>,
) -> Result<Bucket, OpenError> {
    let region = s3_region_for_host(name, Some(auth));
    let credentials = snapshot_credentials_sync(runtime, auth)
        .map_err(OpenError::from)
        .map_err(|e| e.with_path(ctx))?;

    let bucket = Bucket::new(name, region, credentials)
        .map(|b| *b)
        .map_err(std::io::Error::other)
        .map_err(OpenError::from)
        .map_err(|e| e.with_path(ctx))?;

    // A configured region (env/profile) or an access-point alias is trusted as-is.
    // Otherwise discover the bucket's real region instead of assuming one.
    if s3_region_configured(auth) || name.ends_with("-s3alias") || name.ends_with("-ext-s3alias") {
        return Ok(bucket);
    }

    match runtime.block_on(bucket.location()) {
        Ok((actual, _)) if actual != bucket.region() => {
            let credentials = snapshot_credentials_sync(runtime, auth)
                .map_err(OpenError::from)
                .map_err(|e| e.with_path(ctx))?;
            Bucket::new(name, actual, credentials)
                .map(|b| *b)
                .map_err(std::io::Error::other)
                .map_err(OpenError::from)
                .map_err(|e| e.with_path(ctx))
        }
        Ok(_) | Err(_) => Ok(bucket),
    }
}

pub fn source_for_url(
    url: &Url,
    segment: usize,
    runtime: &Runtime,
    s3_auth: Option<&Arc<S3Auth>>,
    io_log: Option<&Arc<IoLog>>,
) -> Result<Box<dyn BytesSource + Send + Sync>, OpenError> {
    match url.scheme() {
        "file" => {
            // url.path() is percent-encoded, so a path with a space or other
            // reserved character (/data/my image.vmdk -> /data/my%20image.vmdk)
            // becomes a spurious ENOENT if used directly. to_file_path decodes
            // it and drops the Windows drive-letter leading slash.
            let path = url
                .to_file_path()
                .map_err(|()| OpenError::from(OpenErrorKind::BadPath(url.to_string())))?;

            let src = FileSource::open(&path)
                .map_err(OpenError::from)
                .map_err(|e| e.with_path(path.to_string_lossy()))?;
            Ok(Box::new(src))
        }
        "s3" => {
            let auth = s3_auth.ok_or_else(|| {
                OpenError::from(std::io::Error::other("S3 credentials not resolved"))
            })?;
            let name = url
                .host_str()
                .ok_or(OpenErrorKind::BadPath(url.to_string()))?;
            // url.path() is percent-encoded, and rust-s3 encodes the key again
            // on the wire, so "my key.e01" would go out as "my%2520key.e01" and
            // never resolve. Decode here so rust-s3 encodes exactly once.
            let key = percent_decode_str(url.path().trim_start_matches('/'))
                .decode_utf8()
                .map_err(|_| OpenError::from(OpenErrorKind::BadPath(url.to_string())))?;
            let bucket = s3_bucket(name, url.as_ref(), runtime, auth)?;

            let (h, _) = runtime
                .block_on(bucket.head_object(key.as_ref()))
                .map_err(std::io::Error::other)
                .map_err(OpenError::from)
                .map_err(|e| e.with_path(url))?;

            // Whatever the endpoint returns, not something we control: a HEAD
            // with no Content-Length, or a negative one, must not panic.
            let len: u64 = h
                .content_length
                .ok_or_else(|| {
                    std::io::Error::new(
                        std::io::ErrorKind::InvalidData,
                        "S3 HEAD returned no Content-Length",
                    )
                })
                .and_then(|len| {
                    len.try_into().map_err(|_| {
                        std::io::Error::new(
                            std::io::ErrorKind::InvalidData,
                            format!("S3 HEAD returned a negative Content-Length: {len}"),
                        )
                    })
                })
                .map_err(OpenError::from)
                .map_err(|e| e.with_path(url))?;
            debug!("content-length: {len}");

            Ok(Box::new(S3Source::new(
                bucket,
                key.into_owned(),
                len,
                auth.clone(),
                segment,
                io_log.cloned(),
            )))
        }
        _ => Err(OpenErrorKind::UnsupportedScheme(url.to_string()).into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use s3::creds::Credentials;

    /// A file whose path contains a space produces a percent-encoded file URL;
    /// opening it must decode the path rather than looking for a literal %20.
    #[test]
    fn file_url_with_space_opens() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("my image.bin");
        std::fs::write(&path, b"hello").unwrap();

        let url = Url::from_file_path(&path).unwrap();
        assert!(url.path().contains("%20"), "url should be percent-encoded");

        let rt = Runtime::new().unwrap();
        let src = source_for_url(&url, 0, &rt, None, None).unwrap();
        assert_eq!(src.end(), 5);
    }

    #[test]
    fn s3_access_point_alias_uses_accesspoint_domain() {
        let region = s3_region_for_host_in_region("foo-s3alias", "us-east-1");
        match &region {
            Region::Custom { region, endpoint } => {
                assert_eq!(region, "us-east-1");
                assert_eq!(endpoint, "s3-accesspoint.us-east-1.amazonaws.com");
            }
            _ => panic!("expected custom access point region"),
        }

        let bucket =
            *Bucket::new("foo-s3alias", region, Credentials::anonymous().unwrap()).unwrap();
        assert_eq!(
            bucket.host(),
            "foo-s3alias.s3-accesspoint.us-east-1.amazonaws.com"
        );
    }
}
