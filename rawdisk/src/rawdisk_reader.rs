use std::{fmt::Debug, io, path::PathBuf, sync::Arc};
use tokio::runtime::Runtime;
use url::Url;

use imagesource::{
    Cache, FoyerCache, IoLog, OpenError, OpenErrorKind, ReadTrace,
    errors::InitError,
    exists::{FileChecker, S3Checker},
    s3_creds::resolve_s3_auth,
    urlsource::{path_or_url_to_url, source_for_url},
};

use crate::seg_path::{DiscoveryError, segment_paths};
use crate::spans::SegmentMap;

// Re-exported so consumers get everything reader-related from this module,
// matching the vmdk-rs/e01-rs API shape.
pub use imagesource::{
    CacheMode, DEFAULT_CACHE_CHUNK_SIZE, DEFAULT_CACHE_FETCH_SIZE, DEFAULT_CACHE_MEM_MIB,
    DEFAULT_S3_CONCURRENCY,
};

/// A reader for raw (dd) disk images. The image is one or more full-cover
/// identity extents -- a single file, or numbered segments (`disk.001`,
/// `disk.002`, ...) treated as one contiguous image -- so reads pass straight
/// through to the cached source(s) at the same offset, crossing segment
/// boundaries via `segments` as needed.
pub struct RawdiskReader {
    /// The path the caller opened. For a split image this is whichever segment
    /// they named, not the image as a whole -- there is no single path for that.
    pub image_path: PathBuf,
    /// Total size across every segment.
    pub image_size: u64,

    segments: SegmentMap,
    cache: Arc<dyn Cache>,
    runtime: Arc<Runtime>,
}

impl Debug for RawdiskReader {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RawdiskReader")
            .field("image_path", &self.image_path)
            .field("image_size", &self.image_size)
            .finish()
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ReadError {
    #[error("Requested offset {0} is beyond end of image {1}")]
    OffsetBeyondEnd(u64, u64),
    #[error("{0}")]
    IoError(#[from] io::Error),
}

#[derive(Debug, thiserror::Error)]
pub enum RawdiskError {
    #[error("{0}")]
    OpenError(#[from] OpenError),
    #[error("{0}")]
    ReadError(#[from] ReadError),
}

#[derive(Debug, Clone)]
pub struct RawdiskReaderOptions {
    /// Foyer backing-cache readahead in chunks (S3/file fetch). 0 disables.
    pub foyer_readahead: usize,
    /// Max concurrent in-flight S3 byte-range fetches. 0 = serial.
    pub s3_concurrency: usize,
    /// Foyer in-memory cache capacity (see [`DEFAULT_CACHE_MEM_MIB`]).
    pub cache_mem_mib: usize,
    /// Cache structure for this session (single vs dedicated-metadata).
    pub cache_mode: CacheMode,
    /// Base directory for foyer's on-disk cache (created as a random subdir
    /// under this path). `None` uses the OS default temp directory.
    pub cache_dir: Option<PathBuf>,
    /// When set, generate JSONL I/O logging (see [`IoLog`]). This will hose performance; only enable it as a diagnostic.
    pub io_log: Option<Arc<IoLog>>,
    /// Foyer block size in bytes: the granularity blocks are stored and evicted at.
    pub cache_chunk_size: usize,
    /// Bytes read from the backing store per miss. When larger than
    /// `cache_chunk_size`, one fetch fills several cache blocks -- worth it
    /// against a high-latency store (S3), wasted bandwidth against a local file.
    /// Defaults to `cache_chunk_size` (no coalescing).
    pub cache_fetch_size: usize,
}

impl Default for RawdiskReaderOptions {
    fn default() -> Self {
        Self {
            foyer_readahead: 0,
            s3_concurrency: DEFAULT_S3_CONCURRENCY,
            cache_mem_mib: DEFAULT_CACHE_MEM_MIB,
            cache_mode: CacheMode::default(),
            cache_dir: None,
            io_log: None,
            cache_chunk_size: DEFAULT_CACHE_CHUNK_SIZE,
            cache_fetch_size: DEFAULT_CACHE_FETCH_SIZE,
        }
    }
}

/// A segment the image needs but that we cannot use, rendered as an open
/// failure. NotFound rather than BadPath: the path the user gave is perfectly
/// well formed, it is the file that is not there (or not usable). The message
/// carries the offending path once, via `OpenError`'s own `{path}: {kind}`.
fn segment_error(path: String, kind: io::ErrorKind, msg: &'static str) -> OpenError {
    OpenError {
        path,
        kind: OpenErrorKind::IoError(io::Error::new(kind, msg)),
    }
}

/// Renders a discovery failure as an open failure.
///
/// The `Undetermined` arm keeps the checker's own cause rather than flattening
/// to NotFound: "we could not tell whether this segment is there" is a different
/// problem from "it is not there", and the difference is what tells an operator
/// whether to retry or to go find the file. It is split into path and cause
/// rather than passed along whole, because `ExistsError` names the path itself
/// and `OpenError` would then print it twice.
fn discovery_error(e: DiscoveryError) -> OpenError {
    match e {
        DiscoveryError::Missing(m) => {
            segment_error(m.path, io::ErrorKind::NotFound, "missing image segment")
        }
        DiscoveryError::Undetermined(e) => OpenError {
            path: e.path,
            kind: OpenErrorKind::IoError(e.source),
        },
    }
}

/// Why we could not resolve `path` to something openable at all.
///
/// `path_or_url_to_url` canonicalises a plain path, and canonicalisation fails
/// when the file is not there -- much the commonest way to land here, and one
/// that "malformed path or URL" describes badly. It also fires before discovery
/// runs, so for local images it pre-empted the "missing image segment" message
/// entirely: only the S3 path could ever reach it.
fn open_target_error(path: &str) -> OpenError {
    match Url::parse(path) {
        // Not a URL at all, so it was meant as a filesystem path.
        Err(url::ParseError::RelativeUrlWithoutBase) => segment_error(
            path.to_string(),
            io::ErrorKind::NotFound,
            "no such image file",
        ),
        _ => OpenError::from(OpenErrorKind::BadPath(path.to_string())),
    }
}

/// Bytes to take from the current segment: the smaller of what is left to fill
/// and what the segment still holds.
///
/// The narrowing happens in `u64`, on the result, never on `segment_left` going
/// in. `segment_left as usize` truncates on a 32-bit target, so a segment with
/// exactly 2^32 bytes remaining would yield 0, `done` would stop advancing, and
/// the read loop would spin forever holding `&self`. Segments that size are
/// ordinary for disk images and rawdisk ships a C API, so 32-bit consumers are
/// not hypothetical. The result is bounded by `remaining`, so the final cast
/// back to `usize` is always exact.
fn take_bytes(remaining: usize, segment_left: u64) -> usize {
    (remaining as u64).min(segment_left) as usize
}

impl RawdiskReader {
    /// Open with default options (single memory-only cache). Used by the C API
    /// and `rawdiskverify`; kept single-arg to match the sibling readers.
    pub fn open<T: AsRef<str>>(image_path: T) -> Result<Self, OpenError> {
        Self::open_with_options(image_path, &RawdiskReaderOptions::default())
    }

    pub fn open_with_options<T: AsRef<str>>(
        image_path: T,
        opts: &RawdiskReaderOptions,
    ) -> Result<Self, OpenError> {
        let url = path_or_url_to_url(&image_path)
            .ok_or_else(|| open_target_error(image_path.as_ref()))?;

        let runtime = Arc::new(
            tokio::runtime::Runtime::new()
                .map_err(InitError::TokioRuntimeFailed)
                .map_err(OpenErrorKind::from)?,
        );

        let cache_chunk_size = opts.cache_chunk_size;
        // Coalesced fetch: one backing-store GET per miss can fill several cache
        // blocks. Never below a block. Pays against a high-latency store (S3).
        let cache_fetch_size = opts.cache_fetch_size.max(cache_chunk_size);
        let c = match opts.cache_mode.clone() {
            CacheMode::SingleMemory => runtime.block_on(FoyerCache::single_memory(
                cache_chunk_size,
                cache_fetch_size,
                opts.cache_mem_mib,
                opts.foyer_readahead,
                opts.s3_concurrency,
                opts.cache_dir.as_deref(),
            )),
            CacheMode::DualHybrid {
                content_disk_mib,
                metadata_mem_mib,
                metadata_disk_mib,
                regular_phase,
            } => runtime.block_on(FoyerCache::dual_hybrid(
                cache_chunk_size,
                cache_fetch_size,
                opts.cache_mem_mib,
                content_disk_mib,
                metadata_mem_mib,
                metadata_disk_mib,
                opts.foyer_readahead,
                opts.s3_concurrency,
                regular_phase,
                opts.cache_dir.as_deref(),
            )),
        }
        .map_err(InitError::CacheSetupFailed)
        .map_err(OpenErrorKind::from)?;

        let cache: Arc<dyn Cache> = Arc::new(c.with_io_log(opts.io_log.clone()));

        let s3_auth = if url.scheme() == "s3" {
            Some(Arc::new(
                resolve_s3_auth(&runtime).map_err(OpenError::from)?,
            ))
        } else {
            None
        };

        // Discovery probes names in the spelling its checker understands:
        // `FileChecker` wants a filesystem path, `S3Checker` a URL. A `file://`
        // URL is neither, and probing one with `FileChecker` makes every
        // candidate look absent, so the image would open as a single segment.
        //
        // Note this tests the INPUT STRING, not `url.scheme()`. `path_or_url_to_url`
        // canonicalises every plain path into a `file://` URL, resolving symlinks
        // on the way, so keying off the parsed scheme would run discovery against
        // the canonical location rather than the names the caller gave -- and
        // segments symlinked into a case directory would open as a single segment.
        let discovery_path = Url::parse(image_path.as_ref())
            .ok()
            .filter(|u| u.scheme() == "file")
            .and_then(|u| u.to_file_path().ok())
            .and_then(|p| p.to_str().map(str::to_string))
            .unwrap_or_else(|| image_path.as_ref().to_string());

        // The no-suffix case short-circuits before any checker is built, and that
        // ordering is load-bearing for S3: `S3Checker::new` calls `s3_bucket`,
        // a GetBucketLocation round trip. Building it unconditionally would add a
        // network call to every single-file S3 open that does not need one.
        let paths = if crate::seg_path::has_numeric_suffix(&discovery_path) {
            match url.scheme() {
                "s3" => {
                    let auth = s3_auth.clone().ok_or_else(|| {
                        OpenErrorKind::BadPath(format!(
                            "{}: s3 URL without resolved credentials",
                            image_path.as_ref()
                        ))
                    })?;
                    let mut checker = S3Checker::new(&url, runtime.clone(), auth)?;
                    segment_paths(&discovery_path, &mut checker)
                }
                _ => segment_paths(&discovery_path, &mut FileChecker),
            }
            .map_err(discovery_error)?
        } else {
            vec![image_path.as_ref().to_string()]
        };

        let mut lengths = Vec::with_capacity(paths.len());
        for (idx, path) in paths.iter().enumerate() {
            let seg_url =
                path_or_url_to_url(path).ok_or_else(|| OpenErrorKind::BadPath(path.clone()))?;
            let src = source_for_url(
                &seg_url,
                idx,
                &runtime,
                s3_auth.as_ref(),
                opts.io_log.as_ref(),
            )?;
            // An empty segment is refused, never skipped: `SegmentMap::locate`
            // steps over zero-length spans to keep the read loop moving, so a
            // stray `touch` or an interrupted copy would otherwise shift every
            // later segment down and serve wrong bytes for the tail of the image
            // with no error at all. A lone empty file is a legitimate, if
            // useless, image and stays acceptable.
            if paths.len() > 1 && src.end() == 0 {
                return Err(segment_error(
                    path.clone(),
                    io::ErrorKind::InvalidData,
                    "empty image segment",
                ));
            }
            lengths.push(src.end());
            cache.add_source(idx, src);
        }

        let segments = SegmentMap::new(lengths);
        let image_size = segments.image_size();

        Ok(Self {
            image_path: image_path.as_ref().into(),
            image_size,
            segments,
            cache,
            runtime,
        })
    }

    /// Takes `&self`: nothing on the read path is mutable, so a reader can serve
    /// concurrent reads without a lock.
    pub fn read_at_offset(&self, offset: u64, mut buf: &mut [u8]) -> Result<usize, ReadError> {
        // don't start reading past the end
        if offset > self.image_size {
            return Err(ReadError::OffsetBeyondEnd(offset, self.image_size));
        }

        // limit the buffer to the image end
        if offset + buf.len() as u64 > self.image_size {
            buf = &mut buf[..(self.image_size - offset) as usize];
        }

        if buf.is_empty() {
            return Ok(0);
        }

        let mut trace = ReadTrace::default();
        let total = buf.len();
        let mut done = 0usize;

        // A read may cross segment boundaries, so serve it in per-segment slices.
        while done < total {
            let want = offset + done as u64;
            let (idx, within, left) = self
                .segments
                .locate(want)
                .ok_or(ReadError::OffsetBeyondEnd(want, self.image_size))?;
            let take = take_bytes(total - done, left);
            // This loop terminates only because `locate` never reports a
            // zero-byte remainder for a live offset, which in turn rests on an
            // invariant established at open (no empty segment in a multi-segment
            // image). That is a long chain to trust silently: `SegmentMap` only
            // debug_asserts it, so a release build with a broken map would spin
            // here forever holding `&self`. Fail loudly instead -- a hung NBD
            // server is far worse to diagnose than an error.
            if take == 0 {
                return Err(ReadError::IoError(io::Error::other(format!(
                    "segment {idx} reported no bytes remaining at image offset {want}"
                ))));
            }
            self.runtime.block_on(self.cache.read(
                idx,
                within,
                &mut buf[done..done + take],
                &mut trace,
            ))?;
            done += take;
        }

        Ok(total)
    }
}

#[cfg(test)]
mod test {
    use super::*;

    fn patterned(n: usize) -> Vec<u8> {
        (0..n).map(|i| ((i * 7 + 3) % 251) as u8).collect()
    }

    fn write_image(dir: &std::path::Path, name: &str, bytes: &[u8]) -> String {
        let p = dir.join(name);
        std::fs::write(&p, bytes).unwrap();
        p.to_str().unwrap().to_string()
    }

    #[test]
    fn read_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let data = patterned(64 * 1024 + 17);
        let path = write_image(dir.path(), "img.raw", &data);

        let mut reader = RawdiskReader::open(&path).unwrap();
        assert_eq!(reader.image_size, data.len() as u64);

        let mut buf = vec![0u8; data.len()];
        let n = reader.read_at_offset(0, &mut buf).unwrap();
        assert_eq!(n, data.len());
        assert_eq!(buf, data);

        // an interior, unaligned read
        let mut buf = vec![0u8; 1000];
        let n = reader.read_at_offset(4321, &mut buf).unwrap();
        assert_eq!(n, 1000);
        assert_eq!(buf, &data[4321..5321]);
    }

    #[test]
    fn read_clamps_to_image_end() {
        let dir = tempfile::tempdir().unwrap();
        let data = patterned(8192);
        let path = write_image(dir.path(), "img.raw", &data);

        let mut reader = RawdiskReader::open(&path).unwrap();

        let mut buf = vec![0xAAu8; 4096];
        let n = reader.read_at_offset(6000, &mut buf).unwrap();
        assert_eq!(n, 2192, "read past end must return only remaining bytes");
        assert_eq!(&buf[..n], &data[6000..]);

        // read exactly at the end returns 0
        let n = reader.read_at_offset(8192, &mut buf).unwrap();
        assert_eq!(n, 0);
    }

    #[test]
    fn read_past_end_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let data = patterned(4096);
        let path = write_image(dir.path(), "img.raw", &data);

        let mut reader = RawdiskReader::open(&path).unwrap();

        let mut buf = vec![0u8; 16];
        match reader.read_at_offset(4097, &mut buf) {
            Err(ReadError::OffsetBeyondEnd(4097, 4096)) => {}
            other => panic!("expected OffsetBeyondEnd, got {other:?}"),
        }
    }

    #[test]
    fn open_nonexistent_path_fails() {
        assert!(RawdiskReader::open("bogus-does-not-exist.raw").is_err());
    }

    /// Naming a segment that is not there is an ordinary mistake and has to read
    /// like one. Canonicalisation fails before discovery ever runs, so this used
    /// to surface as "Malformed path or URL" -- and with an empty path field, so
    /// the message opened with a bare colon.
    #[test]
    fn a_missing_named_segment_reads_as_missing_not_malformed() {
        let dir = tempfile::tempdir().unwrap();
        let whole = patterned(40);
        for (i, n) in ["m.001", "m.002", "m.003", "m.004"].iter().enumerate() {
            write_image(dir.path(), n, &whole[i * 10..i * 10 + 10]);
        }

        let path = dir.path().join("m.005").to_str().unwrap().to_string();
        let err = RawdiskReader::open(&path).unwrap_err().to_string();

        assert!(
            !err.contains("Malformed"),
            "a file that is not there is not a malformed path: {err}"
        );
        assert!(
            !err.starts_with(':'),
            "message begins with a bare colon: {err}"
        );
        assert!(
            err.contains("m.005"),
            "the error should name the file: {err}"
        );
        assert_eq!(err.matches("m.005").count(), 1, "named once: {err}");
    }

    /// The property that matters: a split image reads byte-for-byte the same as
    /// the equivalent single file, including across the boundaries.
    #[test]
    fn split_image_reads_identically_to_a_single_file() {
        let dir = tempfile::tempdir().unwrap();
        let whole = patterned(3000);

        // Deliberately uneven, and deliberately including a one-byte segment:
        // equal segments would let an implementation that derived a single
        // stride from segment 0 pass this test.
        let single = write_image(dir.path(), "whole.raw", &whole);
        write_image(dir.path(), "part.001", &whole[0..1000]);
        write_image(dir.path(), "part.002", &whole[1000..1001]);
        write_image(dir.path(), "part.003", &whole[1001..3000]);
        let split = dir.path().join("part.001").to_str().unwrap().to_string();

        let a = RawdiskReader::open(&single).unwrap();
        let b = RawdiskReader::open(&split).unwrap();
        assert_eq!(b.image_size, 3000);
        assert_eq!(a.image_size, b.image_size);

        // Inside one segment, across both boundaries (the middle segment is a
        // single byte, so anything crossing it touches all three), and the whole
        // image in one call.
        for (off, len) in [(0, 10), (995, 10), (990, 1020), (0, 3000), (2999, 1)] {
            let mut ba = vec![0u8; len];
            let mut bb = vec![0u8; len];
            a.read_at_offset(off, &mut ba).unwrap();
            b.read_at_offset(off, &mut bb).unwrap();
            assert_eq!(ba, bb, "mismatch at offset {off} len {len}");
            assert_eq!(&bb, &whole[off as usize..off as usize + len]);
        }
    }

    /// Naming any segment must give the whole image, not one starting partway in.
    #[test]
    fn opening_a_later_segment_still_gives_the_whole_image() {
        let dir = tempfile::tempdir().unwrap();
        let whole = patterned(300);
        write_image(dir.path(), "p.001", &whole[0..100]);
        write_image(dir.path(), "p.002", &whole[100..200]);
        write_image(dir.path(), "p.003", &whole[200..300]);

        let path = dir.path().join("p.002").to_str().unwrap().to_string();
        let r = RawdiskReader::open(&path).unwrap();
        assert_eq!(r.image_size, 300);

        let mut buf = vec![0u8; 300];
        r.read_at_offset(0, &mut buf).unwrap();
        assert_eq!(buf, whole);
    }

    #[test]
    fn a_missing_middle_segment_refuses_to_open() {
        let dir = tempfile::tempdir().unwrap();
        let whole = patterned(300);
        write_image(dir.path(), "g.001", &whole[0..100]);
        write_image(dir.path(), "g.003", &whole[200..300]);

        let path = dir.path().join("g.003").to_str().unwrap().to_string();
        let err = RawdiskReader::open(&path).unwrap_err().to_string();
        assert!(err.contains("g.002"), "error should name the gap: {err}");
        assert!(
            !err.contains("Malformed"),
            "a missing file is not a malformed path: {err}"
        );
        assert_eq!(
            err.matches("g.002").count(),
            1,
            "the missing file should be named once: {err}"
        );
    }

    /// The read loop only terminates if every step takes a positive number of
    /// bytes. A segment remainder at or above 2^32 must not narrow to zero --
    /// `left as usize` did exactly that on a 32-bit target, and the loop spun
    /// forever. Not reproducible on a 64-bit host, so the shape is pinned here.
    #[test]
    fn take_bytes_never_narrows_a_live_remainder_to_zero() {
        for segment_left in [1u64 << 32, (1u64 << 32) + 1, u32::MAX as u64 + 1, u64::MAX] {
            assert_eq!(take_bytes(4096, segment_left), 4096, "{segment_left}");
            assert_ne!(take_bytes(1, segment_left), 0, "{segment_left}");
        }
        // And it still clamps to the segment when the segment is the smaller one.
        assert_eq!(take_bytes(4096, 100), 100);
        assert_eq!(take_bytes(100, 4096), 100);
        assert_eq!(take_bytes(0, 4096), 0);
    }

    /// Whichever kind of discovery failure it is, the offending segment gets
    /// named exactly once. `ExistsError` carries the path itself, so passing it
    /// along whole would have `OpenError` print it a second time.
    #[test]
    fn a_discovery_failure_names_the_segment_once() {
        use crate::seg_path::MissingSegment;
        use imagesource::exists::ExistsError;

        let missing = discovery_error(DiscoveryError::Missing(MissingSegment {
            path: "/img/d.002".into(),
        }))
        .to_string();
        assert_eq!(missing.matches("/img/d.002").count(), 1, "{missing}");

        let undetermined = discovery_error(DiscoveryError::Undetermined(ExistsError::new(
            "/img/d.003",
            io::Error::other("HEAD returned HTTP 503"),
        )))
        .to_string();
        assert_eq!(
            undetermined.matches("/img/d.003").count(),
            1,
            "{undetermined}"
        );
        assert!(
            undetermined.contains("503"),
            "the cause must survive: {undetermined}"
        );
    }

    /// An empty segment must be refused, not silently skipped: skipping it
    /// shifts every later segment down and serves wrong bytes for the tail of
    /// the image.
    #[test]
    fn a_zero_length_segment_refuses_to_open() {
        let dir = tempfile::tempdir().unwrap();
        let whole = patterned(200);
        write_image(dir.path(), "z.001", &whole[0..100]);
        write_image(dir.path(), "z.002", &[]);
        write_image(dir.path(), "z.003", &whole[100..200]);

        let path = dir.path().join("z.001").to_str().unwrap().to_string();
        let err = RawdiskReader::open(&path).unwrap_err().to_string();
        assert!(
            err.contains("z.002"),
            "error should name the empty segment: {err}"
        );
    }

    /// The same image named two ways must not give two different images.
    /// Discovery probes the filesystem, so a `file://` URL has to be normalised
    /// back to a path first or every candidate looks absent.
    #[test]
    fn file_url_and_plain_path_find_the_same_segments() {
        let dir = tempfile::tempdir().unwrap();
        let whole = patterned(200);
        let plain = write_image(dir.path(), "u.001", &whole[0..100]);
        write_image(dir.path(), "u.002", &whole[100..200]);

        let url = url::Url::from_file_path(std::fs::canonicalize(&plain).unwrap())
            .unwrap()
            .to_string();
        assert!(url.starts_with("file://"), "{url}");

        let a = RawdiskReader::open(&plain).unwrap();
        let b = RawdiskReader::open(&url).unwrap();
        assert_eq!(a.image_size, 200);
        assert_eq!(
            a.image_size, b.image_size,
            "file:// URL saw a different image"
        );

        // A read spanning the segment boundary, both ways.
        let mut ba = vec![0u8; 200];
        let mut bb = vec![0u8; 200];
        a.read_at_offset(0, &mut ba).unwrap();
        b.read_at_offset(0, &mut bb).unwrap();
        assert_eq!(ba, whole);
        assert_eq!(ba, bb);
    }

    /// Symlinking segments into a case directory -- an ordinary forensic
    /// workflow -- must discover them by the names the caller gave, not by the
    /// canonical (symlink-resolved) path. Regression: normalising discovery off
    /// `url.scheme() == "file"` fires for plain paths too, since
    /// `path_or_url_to_url` canonicalises every path into a `file://` URL,
    /// resolving symlinks along the way -- so discovery ran on the store path,
    /// found only one segment there, and the image opened as if unsplit.
    #[cfg(unix)]
    #[test]
    fn symlinked_segments_are_discovered_by_their_link_names() {
        use std::os::unix::fs::symlink;

        let store = tempfile::tempdir().unwrap();
        let case = tempfile::tempdir().unwrap();
        let whole = patterned(200);

        write_image(store.path(), "first.dd", &whole[0..100]);
        write_image(store.path(), "second.dd", &whole[100..200]);

        symlink(store.path().join("first.dd"), case.path().join("a.001")).unwrap();
        symlink(store.path().join("second.dd"), case.path().join("a.002")).unwrap();

        let path = case.path().join("a.001").to_str().unwrap().to_string();
        let r = RawdiskReader::open(&path).unwrap();
        assert_eq!(
            r.image_size, 200,
            "symlinked segments must be discovered by their link names, not the canonical path"
        );

        let mut buf = vec![0u8; 200];
        r.read_at_offset(0, &mut buf).unwrap();
        assert_eq!(buf, whole);
    }

    /// Unsuffixed images must not regress.
    #[test]
    fn unsuffixed_image_is_still_a_single_segment() {
        let dir = tempfile::tempdir().unwrap();
        let whole = patterned(500);
        let path = write_image(dir.path(), "plain.raw", &whole);

        let r = RawdiskReader::open(&path).unwrap();
        assert_eq!(r.image_size, 500);
        let mut buf = vec![0u8; 500];
        r.read_at_offset(0, &mut buf).unwrap();
        assert_eq!(buf, whole);
    }
}
