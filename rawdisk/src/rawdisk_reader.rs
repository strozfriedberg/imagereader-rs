use std::{fmt::Debug, io, path::PathBuf, sync::Arc};
use tokio::runtime::Runtime;

use imagesource::{
    Cache, FoyerCache, IoLog, OpenError, OpenErrorKind, ReadTrace,
    errors::InitError,
    s3_creds::resolve_s3_auth,
    urlsource::{path_or_url_to_url, source_for_url},
};

// Re-exported so consumers get everything reader-related from this module,
// matching the vmdk-rs/e01-rs API shape.
pub use imagesource::{
    CacheMode, DEFAULT_CACHE_CHUNK_SIZE, DEFAULT_CACHE_MEM_MIB, DEFAULT_S3_CONCURRENCY,
};

/// A reader for raw (dd) disk images. The image is a single full-cover
/// identity extent, so reads pass straight through to the cached source at
/// the same offset.
pub struct RawdiskReader {
    pub image_path: PathBuf,
    pub image_size: u64,

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
    /// Foyer block size in bytes.
    pub cache_chunk_size: usize,
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
        }
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
        let c = match opts.cache_mode.clone() {
            CacheMode::SingleMemory => runtime.block_on(FoyerCache::single_memory(
                cache_chunk_size,
                // No fetch coalescing here: it pays only against a high-latency
                // store, and e01 is the one served from S3 today.
                cache_chunk_size,
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
                cache_chunk_size,
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

        // A raw image is one segment: source index 0, identity mapping.
        let src = source_for_url(&url, 0, &runtime, s3_auth.as_ref(), opts.io_log.as_ref())?;
        let image_size = src.end();
        cache.add_source(0, src);

        Ok(Self {
            image_path: image_path.as_ref().into(),
            image_size,
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
        self.runtime
            .block_on(self.cache.read(0, offset, buf, &mut trace))?;

        Ok(buf.len())
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
}
