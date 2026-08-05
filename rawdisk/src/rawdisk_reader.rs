use std::{fmt::Debug, io, path::PathBuf, sync::Arc};
use tokio::runtime::Runtime;

use imagesource::{
    Cache, FoyerCache, IoLog, OpenError, OpenErrorKind, ReadTrace,
    errors::InitError,
    exists::{FileChecker, S3Checker},
    s3_creds::resolve_s3_auth,
    urlsource::{path_or_url_to_url, source_for_url},
};

use crate::seg_path::{MissingSegment, segment_paths};
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

fn missing_segment_error(e: MissingSegment) -> OpenError {
    OpenError {
        path: e.path.clone(),
        kind: OpenErrorKind::BadPath(e.to_string()),
    }
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
            .ok_or(OpenErrorKind::BadPath(image_path.as_ref().into()))?;

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

        // A raw image may be split across numbered segments. Discovery works on
        // the original path rather than the URL, because that is what the user
        // named and what the checkers probe.
        //
        // The no-suffix case short-circuits before any checker is built, and that
        // ordering is load-bearing for S3: `S3Checker::new` calls `s3_bucket`,
        // which is a GetBucketLocation round trip. Building it unconditionally
        // would add a network call to every single-file S3 open that does not
        // need one.
        let paths = if crate::seg_path::has_numeric_suffix(image_path.as_ref()) {
            match url.scheme() {
                "s3" => {
                    let auth = s3_auth.clone().ok_or_else(|| {
                        OpenErrorKind::BadPath(format!(
                            "{}: s3 URL without resolved credentials",
                            image_path.as_ref()
                        ))
                    })?;
                    let mut checker = S3Checker::new(&url, runtime.clone(), auth)?;
                    segment_paths(image_path.as_ref(), &mut checker)
                }
                _ => segment_paths(image_path.as_ref(), &mut FileChecker),
            }
            .map_err(missing_segment_error)?
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
        // `locate` never reports a zero-byte remainder for a live offset, so this
        // loop always makes progress.
        while done < total {
            let want = offset + done as u64;
            let (idx, within, left) = self
                .segments
                .locate(want)
                .ok_or(ReadError::OffsetBeyondEnd(want, self.image_size))?;
            let take = (total - done).min(left as usize);
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

    /// The property that matters: a split image reads byte-for-byte the same as
    /// the equivalent single file, including across the boundaries.
    #[test]
    fn split_image_reads_identically_to_a_single_file() {
        let dir = tempfile::tempdir().unwrap();
        let whole = patterned(3000);

        let single = write_image(dir.path(), "whole.raw", &whole);
        write_image(dir.path(), "part.001", &whole[0..1000]);
        write_image(dir.path(), "part.002", &whole[1000..2000]);
        write_image(dir.path(), "part.003", &whole[2000..3000]);
        let split = dir.path().join("part.001").to_str().unwrap().to_string();

        let a = RawdiskReader::open(&single).unwrap();
        let b = RawdiskReader::open(&split).unwrap();
        assert_eq!(b.image_size, 3000);
        assert_eq!(a.image_size, b.image_size);

        // Inside one segment, across one boundary, across two boundaries, and
        // the whole image in one call.
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
