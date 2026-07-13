use kaitai::{BytesReader, KError, ReadSeek};
use rayon::prelude::*;
use s3::{bucket::Bucket, region::Region};
use std::{
    fmt::Debug,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};
use tokio::runtime::Runtime;
use tracing::{debug, warn};
use url::{self, Url};

use crate::{
    cacheworkersource::CacheWorkerSource,
    error::{IoError, LibError},
    readworker::{DecodedChunkCache, ReadWorker},
    sec_read::{Chunk, Section, SectionIterator, VolumeSection},
    seg_path::{ExistsChecker, UnrecognizedExtension, validated_segment_paths},
    segment::SegmentFileHeader,
};
use imagesource::{
    Cache, CacheReadSeek, FoyerCache, IoLog, ReadTimer, ReadTrace, chunk_cache_label,
    s3_creds::{S3Auth, resolve_s3_auth, snapshot_credentials_sync},
    urlsource::{path_or_url_to_url, s3_bucket, source_for_url},
};

// Re-exported so existing consumers keep their `e01::e01_reader::…` paths.
pub use imagesource::{CacheMode, InitError};

#[derive(Debug, thiserror::Error)]
pub enum OpenError {
    #[error("{0}")]
    PathGlobError(#[from] UnrecognizedExtension),
    #[error("No segment files given")]
    NoSegmentFiles,
    #[error("Missing volume section in {0}")]
    MissingVolumeSection(PathBuf),
    #[error(
        "Invalid volume geometry: {sectors_per_chunk} sectors per chunk, {bytes_per_sector} bytes per sector"
    )]
    InvalidVolumeGeometry {
        sectors_per_chunk: u32,
        bytes_per_sector: u32,
    },
    #[error("Too many chunks found: actual {0}, expected {1}")]
    TooManyChunks(usize, usize),
    #[error("Too few chunks found: actual {0}, expected {1}")]
    TooFewChunks(usize, usize),
    #[error("Error reading {path}: {source}")]
    IoError {
        path: String,
        #[source]
        source: LibError,
    },
    #[error("Bad data in {path}: {source}")]
    BadData {
        path: String,
        #[source]
        source: LibError,
    },
    #[error("Malformed path or URL: {0}")]
    BadPath(String),
    #[error("Unsupported URL scheme: {0}")]
    UnsupportedScheme(String),
    #[error("{0}")]
    InitializationFailed(#[from] InitError),
    #[error("{0}")]
    Source(#[from] imagesource::OpenError),
    #[error("Segment file {path} has segment number {actual}, expected {expected}")]
    SegmentOutOfOrder {
        path: String,
        actual: u16,
        expected: u16,
    },
}

impl From<std::io::Error> for OpenError {
    fn from(e: std::io::Error) -> Self {
        OpenError::from(LibError::from(IoError::from(e)))
    }
}

impl From<LibError> for OpenError {
    fn from(e: LibError) -> Self {
        match e {
            LibError::IoError(_) => Self::IoError {
                path: "".into(), // set using with_path()
                source: e,
            },
            _ => Self::BadData {
                path: "".into(), // set using with_path()
                source: e,
            },
        }
    }
}

impl From<KError> for OpenError {
    fn from(e: KError) -> Self {
        Self::IoError {
            path: "".into(), // set using with_path()
            source: LibError::IoError(IoError::Read(e)),
        }
    }
}

impl OpenError {
    fn with_path<T: AsRef<str>>(self, path: T) -> Self {
        match self {
            Self::IoError { source, .. } => Self::IoError {
                path: path.as_ref().into(),
                source,
            },
            Self::BadData { source, .. } => Self::BadData {
                path: path.as_ref().into(),
                source,
            },
            _ => self,
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ReadErrorKind {
    #[error("Requested offset {0} is beyond end of image {1}")]
    OffsetBeyondEnd(u64, u64),
    #[error("{0}")]
    IoError(#[from] std::io::Error),
    #[error("Chunk {0} is {1} bytes long, must be at least 5 bytes long")]
    TooShort(usize, usize),
    #[error("Chunk {0} checksum failed: calculated {1}, expected {2}")]
    BadChecksum(usize, u32, u32),
    #[error("Decompression of chunk {0} failed: {1}")]
    DecompressionFailed(usize, #[source] std::io::Error),
}

#[derive(Debug, thiserror::Error)]
#[error(
    "{}{}{source}",
    path.as_deref().unwrap_or(Path::new("")).display(),
    path.as_ref().map(|_| ": ").unwrap_or("")
)]
pub struct ReadError {
    path: Option<PathBuf>,
    #[source]
    source: ReadErrorKind,
}

impl ReadError {
    fn with_path<T: AsRef<Path>>(self, path: T) -> Self {
        Self {
            path: Some(path.as_ref().into()),
            source: self.source,
        }
    }
}

impl From<ReadErrorKind> for ReadError {
    fn from(e: ReadErrorKind) -> Self {
        Self {
            path: None,
            source: e,
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum E01Error {
    #[error("{0}")]
    OpenError(#[from] OpenError),
    #[error("{0}")]
    ReadError(#[from] ReadError),
}

#[derive(Debug)]
struct Segment {
    pub path: String,
}

struct SegmentComponents {
    path: String,
    segment_number: u16,
    volume: Option<VolumeSection>,
    md5: Option<[u8; 16]>,
    sha1: Option<[u8; 20]>,
    chunks: Vec<Chunk>,
    done: bool,
}

fn read_segment<T: AsRef<str>>(
    segment_path: T,
    segment_index: usize,
    io: &BytesReader,
    ignore_checksums: bool,
) -> Result<SegmentComponents, OpenError> {
    debug!("reading sections {}", segment_path.as_ref());

    let header = SegmentFileHeader::new(io)
        .map_err(OpenError::from)
        .map_err(|e| e.with_path(&segment_path))?;

    let mut done = false;

    // we can't reserve capacity for chunks because we don't know how many
    // chunks are in a segment until we read all its table sections
    let mut chunks = vec![];

    let mut end_of_sectors = 0;

    let mut volume = None;
    let mut md5 = None;
    let mut sha1 = None;

    let mut sections = SectionIterator::new(io, ignore_checksums);

    for section in sections.by_ref() {
        let section = section
            .map_err(OpenError::from)
            .map_err(|e| e.with_path(&segment_path))?;

        debug!("found section {section:?}");

        match section {
            Section::Volume(v) => volume = Some(v),
            Section::Table(t) if !t.is_empty() => {
                chunks.extend(t);
                // set the end of the last chunk in the table
                let chunks_len = chunks.len();
                chunks[chunks_len - 1].end_offset = end_of_sectors;
            }
            Section::Sectors(eos) => end_of_sectors = eos,
            Section::Hash(h) => md5 = Some(h),
            Section::Digest(d_md5, d_sha1) => {
                md5 = Some(d_md5);
                sha1 = Some(d_sha1);
            }
            Section::Done => {
                done = true;
                break;
            }
            _ => {}
        }
    }

    if done && sections.next().is_some() {
        warn!("more sections after done");
    }

    // set the segment index for these chunks
    for c in &mut chunks {
        c.segment = segment_index;
    }

    Ok(SegmentComponents {
        path: segment_path.as_ref().into(),
        segment_number: header.segment_number(),
        volume,
        md5,
        sha1,
        chunks,
        done,
    })
}

fn make_bytes_reader(
    p: &str,
    idx: usize,
    cache: Arc<dyn Cache>,
    runtime: Arc<Runtime>,
    s3_auth: Option<&Arc<S3Auth>>,
    io_log: Option<Arc<IoLog>>,
) -> Result<BytesReader, OpenError> {
    debug!("opening {}", p);

    let url = path_or_url_to_url(p).ok_or(OpenError::BadPath(p.into()))?;

    let src = source_for_url(&url, idx, &runtime, s3_auth, io_log.as_ref())?;

    let seg_len = src.end();
    cache.add_source(idx, src);

    let crs = CacheReadSeek::new(cache, runtime, idx, seg_len, None);

    // Kaitai's generated struct parser issues reads a few bytes at a time
    // (one per primitive field) while walking segment headers/tables.
    // Buffering coalesces those into far fewer round trips through the
    // cache, which otherwise serializes every tiny read behind a lock.
    let buffered = std::io::BufReader::with_capacity(1024 * 1024, crs);

    let rs = Box::new(buffered) as Box<dyn ReadSeek>;

    BytesReader::try_from(rs)
        .map_err(OpenError::from)
        .map_err(|e| e.with_path(p))
}

struct E01Metadata {
    volume: VolumeSection,
    md5: Option<[u8; 16]>,
    sha1: Option<[u8; 20]>,
    segments: Vec<Segment>,
    segment_paths: Vec<PathBuf>,
    chunks: Vec<Chunk>,
}

fn process_segments<S: IntoIterator<Item = SegmentComponents>>(
    segs: S,
) -> Result<E01Metadata, OpenError> {
    let mut volume = None;
    let mut stored_md5 = None;
    let mut stored_sha1 = None;

    let mut segments = vec![];
    let mut segment_paths = vec![];
    let mut chunks = vec![];

    let mut done = false;

    for (i, seg) in segs.into_iter().enumerate() {
        let expected = (i + 1) as u16; // EWF segment numbers are 1-based
        if seg.segment_number != expected {
            return Err(OpenError::SegmentOutOfOrder {
                path: seg.path.clone(),
                actual: seg.segment_number,
                expected,
            });
        }

        debug!("handling {}", seg.path);

        // take the volume section if it's the first one
        match (seg.volume, &volume) {
            // we have no volume section, and saw one
            (Some(sv), None) => {
                // we can size the chunks vec now
                let unread_chunks = (sv.chunk_count as usize).saturating_sub(chunks.len());
                chunks.reserve_exact(unread_chunks);
                volume = Some(sv);
            }
            // we have a volume section, and didn't see a new one
            (None, Some(_)) => {}
            // we have no volume section, and saw none;
            // this can happen only on the first segment
            (None, None) => return Err(OpenError::MissingVolumeSection((&seg.path).into())),
            // we have a volume section and saw another one!
            (Some(_), Some(_)) => warn!("duplicate volume section"),
        }

        // take the stored MD5 if it's the first one
        match (seg.md5, &stored_md5) {
            (Some(h), None) => stored_md5 = Some(h),
            (Some(new), Some(old)) if new != *old => warn!("duplicate stored MD5s disagree"),
            _ => {}
        }

        // take the stored SHA1 if it's the first one
        match (seg.sha1, &stored_sha1) {
            (Some(h), None) => stored_sha1 = Some(h),
            (Some(new), Some(old)) if new != *old => warn!("duplicate stored SHA1s disagree"),
            _ => {}
        }

        // record the chunks
        chunks.extend(seg.chunks);

        // record the segment
        segment_paths.push((&seg.path).into());
        segments.push(Segment { path: seg.path });

        if seg.done {
            if done {
                warn!("more segments after finding done section");
            } else {
                done = true;
            }
        }
    }

    if !done {
        warn!("read all segments without finding done section");
    }

    let volume = volume.expect("volume section must have been found");

    Ok(E01Metadata {
        volume,
        md5: stored_md5,
        sha1: stored_sha1,
        segments,
        segment_paths,
        chunks,
    })
}

/// `sectors_per_chunk` and `bytes_per_sector` are read straight out of the
/// image's volume section, and `read_at_offset` divides by their product. A
/// crafted image declaring either as zero would otherwise panic the reader with
/// a divide-by-zero on the first read.
fn validate_volume(volume: &VolumeSection) -> Result<(), OpenError> {
    if volume.chunk_size() == 0 {
        return Err(OpenError::InvalidVolumeGeometry {
            sectors_per_chunk: volume.sectors_per_chunk,
            bytes_per_sector: volume.bytes_per_sector,
        });
    }
    Ok(())
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub enum CorruptSectionPolicy {
    #[default]
    Error,
    DamnTheTorpedoes,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub enum CorruptChunkPolicy {
    Error,
    #[default]
    Zero,
    RawIfPossible,
}

/// Default concurrent S3 segment fetches (foyer cache misses).
pub const DEFAULT_S3_CONCURRENCY: usize = 8;

/// Threads for the chunk-decompression pool.
///
/// Measured by sweeping `e01verify --parallel-threads N` over a 28 GiB image on
/// a 32-core machine (wall / total CPU):
///
/// ```text
///   32 (one per core)   64.4s / 484s      2 threads   63.5s / 152s
///   16                  62.6s / 303s      serial      81.9s / 148s
///    8                  61.0s / 193s
///    4                  60.7s / 162s   <-- fastest, and 3x less CPU than 32
/// ```
///
/// Four threads beat thirty-two on wall clock *and* used a third of the CPU. A
/// 1 MiB read is only ~1-2ms of inflate; splitting it 32 ways gives each worker
/// ~30us of work, and waking and parking them costs more than the work does
/// (sys time: 2m58 at 32 threads, 13s at 4).
pub const DEFAULT_PARALLEL_CHUNK_THREADS: usize = 4;

/// Default foyer memory cache capacity (~1 MiB entries when chunk size is 1 MiB).
pub const DEFAULT_CACHE_MEM_MIB: usize = 1024;

#[derive(Debug, Clone)]
pub struct E01ReaderOptions {
    pub corrupt_section_policy: CorruptSectionPolicy,
    pub corrupt_chunk_policy: CorruptChunkPolicy,
    /// Foyer backing-cache readahead in 1 MiB blocks (S3/file segment fetch). 0 disables.
    pub foyer_readahead: usize,
    /// Max concurrent in-flight S3 segment byte-range fetches. 0 = serial.
    pub s3_concurrency: usize,
    /// Foyer in-memory cache capacity in ~1 MiB entries (see [`DEFAULT_CACHE_MEM_MIB`]).
    pub cache_mem_mib: usize,
    /// Cache structure for this session (single vs dedicated-metadata).
    pub cache_mode: CacheMode,
    /// Base directory for foyer's on-disk cache (created as a random subdir
    /// under this path). `None` uses the OS default temp directory.
    pub cache_dir: Option<PathBuf>,
    /// When set, generate JSONL I/O logging (see [`IoLog`]). This will hose performance; only enable it as a diagnostic.
    pub io_log: Option<Arc<IoLog>>,
    /// Threads to use for `parallel_chunk_reads`. 0 uses rayon's global pool
    /// (one thread per core).
    ///
    /// A 1 MiB read is 32 chunks of ~1-2ms of total decompression. Handing that
    /// to one thread per core gives each ~30us of work and then parks them all
    /// again; the park/unpark futexes cost more than the inflate does. See
    /// [`DEFAULT_PARALLEL_CHUNK_THREADS`] for the sweep.
    pub parallel_chunk_threads: usize,

    /// Decompress a multi-chunk read's chunks in parallel, over rayon.
    ///
    /// With [`DEFAULT_PARALLEL_CHUNK_THREADS`], a whole-image verify of a 28 GiB
    /// image takes 60.7s using 162s of CPU, against 81.9s and 148s serially --
    /// 26% off the wall clock for 9% more CPU. (Before the pool was bounded this
    /// was 3.3x the CPU, nearly all of it kernel time spent waking and parking
    /// 32 threads to do 30us of work each.)
    pub parallel_chunk_reads: bool,
}

impl Default for E01ReaderOptions {
    fn default() -> Self {
        Self {
            corrupt_section_policy: CorruptSectionPolicy::default(),
            corrupt_chunk_policy: CorruptChunkPolicy::default(),
            foyer_readahead: 0,
            s3_concurrency: DEFAULT_S3_CONCURRENCY,
            cache_mem_mib: DEFAULT_CACHE_MEM_MIB,
            cache_mode: CacheMode::default(),
            cache_dir: None,
            io_log: None,
            parallel_chunk_reads: true,
            parallel_chunk_threads: DEFAULT_PARALLEL_CHUNK_THREADS,
        }
    }
}

pub struct E01Reader {
    segments: Vec<Segment>,
    chunks: Vec<Chunk>,

    pub chunk_size: usize,
    pub chunk_count: usize,
    pub sector_size: usize,
    pub sector_count: usize,
    pub image_size: u64,

    pub stored_md5: Option<[u8; 16]>,
    pub stored_sha1: Option<[u8; 20]>,

    pub segment_paths: Vec<PathBuf>,

    corrupt_section_policy: CorruptSectionPolicy,
    corrupt_chunk_policy: CorruptChunkPolicy,

    workers: Vec<ReadWorker>,
    cache: Arc<dyn Cache>,
    decoded_chunk_cache: Arc<Mutex<DecodedChunkCache>>,
    runtime: Arc<Runtime>,
    io_log: Option<Arc<IoLog>>,
    parallel_chunk_reads: bool,
    /// `None` uses rayon's global pool.
    chunk_pool: Option<Arc<rayon::ThreadPool>>,
}

const DECODED_CHUNK_CACHE_CHUNKS: usize = 1024;

/// One chunk's worth of work: which chunk, where to read it from, and where its
/// bytes go in the caller's buffer.
#[allow(clippy::type_complexity)]
type ChunkTask<'a> = (
    usize,
    &'a Chunk,
    CacheWorkerSource,
    Arc<Mutex<DecodedChunkCache>>,
    &'a mut [u8],
    usize,
    usize,
    &'a String,
    &'a mut ReadWorker,
);

/// A bounded pool for chunk decompression, or `None` to use rayon's global one.
fn build_chunk_pool(threads: usize) -> Result<Option<Arc<rayon::ThreadPool>>, OpenError> {
    if threads == 0 {
        return Ok(None);
    }

    rayon::ThreadPoolBuilder::new()
        .num_threads(threads)
        .thread_name(|i| format!("e01-chunk-{i}"))
        .build()
        .map(|p| Some(Arc::new(p)))
        .map_err(|e| OpenError::from(std::io::Error::other(e)))
}

fn run_chunk_task(task: ChunkTask<'_>) -> Result<(), ReadError> {
    let (
        chunk_index,
        chunk,
        mut src,
        decoded_chunk_cache,
        sbuf,
        beg_in_chunk,
        end_in_chunk,
        seg_path,
        worker,
    ) = task;

    worker
        .read_cached(
            chunk,
            &mut src,
            chunk_index,
            sbuf,
            beg_in_chunk,
            end_in_chunk,
            &decoded_chunk_cache,
        )
        .map_err(ReadError::from)
        .map_err(|e| e.with_path(seg_path))
}

impl Debug for E01Reader {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("E01Reader")
            .field("segments", &self.segments)
            .field("chunks", &self.chunks)
            .field("chunk_size", &self.chunk_size)
            .field("chunk_count", &self.chunk_count)
            .field("sector_size", &self.sector_size)
            .field("sector_count", &self.sector_count)
            .field("image_size", &self.image_size)
            .field("stored_md5", &self.stored_md5)
            .field("stored_sha1", &self.stored_sha1)
            .field("segment_paths", &self.segment_paths)
            .field("corrupt_section_policy", &self.corrupt_section_policy)
            .field("corrupt_chunk_policy", &self.corrupt_chunk_policy)
            .finish()
    }
}

struct FileChecker;

impl ExistsChecker for FileChecker {
    fn exists<T: AsRef<str>>(&mut self, path: T) -> bool {
        Path::new(path.as_ref()).is_file()
    }
}

struct S3Checker {
    bucket_name: String,
    region: Region,
    runtime: Arc<Runtime>,
    auth: Arc<S3Auth>,
}

impl S3Checker {
    fn new(url: &Url, runtime: Arc<Runtime>, auth: Arc<S3Auth>) -> Result<Self, OpenError> {
        let name = url.host_str().ok_or(OpenError::BadPath(url.to_string()))?;
        let bucket = s3_bucket(name, url.as_ref(), &runtime, &auth)?;
        Ok(Self {
            bucket_name: name.to_string(),
            region: bucket.region().clone(),
            runtime,
            auth,
        })
    }
}

impl ExistsChecker for S3Checker {
    fn exists<T: AsRef<str>>(&mut self, path: T) -> bool {
        Url::parse(path.as_ref())
            .map(|url| {
                let bucket = snapshot_credentials_sync(&self.runtime, &self.auth)
                    .ok()
                    .and_then(|credentials| {
                        Bucket::new(&self.bucket_name, self.region.clone(), credentials)
                            .ok()
                            .map(|b| *b)
                    });
                bucket
                    .map(|bucket| {
                        self.runtime
                            .block_on(bucket.head_object(url.path().trim_start_matches('/')))
                            .is_ok_and(|(_, code)| code == 200)
                    })
                    .unwrap_or(false)
            })
            .unwrap_or(false)
    }
}

impl E01Reader {
    pub fn open_glob<T: AsRef<str>>(
        example_segment_path: T,
        options: &E01ReaderOptions,
    ) -> Result<Self, OpenError> {
        let url = path_or_url_to_url(&example_segment_path)
            .ok_or(OpenError::BadPath(example_segment_path.as_ref().into()))?;

        let runtime =
            Arc::new(tokio::runtime::Runtime::new().map_err(InitError::TokioRuntimeFailed)?);

        match url.scheme() {
            "file" => Self::open_impl(
                validated_segment_paths(example_segment_path, FileChecker)?,
                options,
                runtime,
                None,
            ),
            "s3" => {
                let s3_auth = resolve_s3_auth(&runtime).map_err(OpenError::from)?;
                let auth = Arc::new(s3_auth);
                Self::open_impl(
                    validated_segment_paths(
                        example_segment_path,
                        S3Checker::new(&url, runtime.clone(), auth.clone())?,
                    )?,
                    options,
                    runtime,
                    Some(auth),
                )
            }
            _ => Err(OpenError::UnsupportedScheme(url.to_string())),
        }
    }

    pub fn open<T: IntoIterator<Item: AsRef<str>>>(
        segment_paths: T,
        options: &E01ReaderOptions,
    ) -> Result<Self, OpenError> {
        let runtime =
            Arc::new(tokio::runtime::Runtime::new().map_err(InitError::TokioRuntimeFailed)?);

        let paths: Vec<String> = segment_paths
            .into_iter()
            .map(|p| p.as_ref().to_string())
            .collect();
        let s3_auth = if paths.iter().any(|p| p.starts_with("s3://")) {
            Some(Arc::new(
                resolve_s3_auth(&runtime).map_err(OpenError::from)?,
            ))
        } else {
            None
        };

        Self::open_impl(paths, options, runtime, s3_auth)
    }

    fn open_impl<T: IntoIterator<Item: AsRef<str>>>(
        segment_paths: T,
        options: &E01ReaderOptions,
        runtime: Arc<Runtime>,
        s3_auth: Option<Arc<S3Auth>>,
    ) -> Result<Self, OpenError> {
        let mut sp_itr = segment_paths.into_iter().peekable();

        if sp_itr.peek().is_none() {
            return Err(OpenError::NoSegmentFiles);
        }

        let cache_chunk_size = 1024 * 1024;
        let cache_mem_size = options.cache_mem_mib;
        let foyer_readahead = options.foyer_readahead;
        let s3_concurrency = options.s3_concurrency;
        let c = match options.cache_mode.clone() {
            CacheMode::SingleMemory => runtime
                .block_on(FoyerCache::single_memory(
                    cache_chunk_size,
                    cache_mem_size,
                    foyer_readahead,
                    s3_concurrency,
                    options.cache_dir.as_deref(),
                ))
                .map_err(InitError::CacheSetupFailed)?,
            CacheMode::DualHybrid {
                content_disk_mib,
                metadata_mem_mib,
                metadata_disk_mib,
                regular_phase,
            } => runtime
                .block_on(FoyerCache::dual_hybrid(
                    cache_chunk_size,
                    cache_mem_size,
                    content_disk_mib,
                    metadata_mem_mib,
                    metadata_disk_mib,
                    foyer_readahead,
                    s3_concurrency,
                    regular_phase,
                    options.cache_dir.as_deref(),
                ))
                .map_err(InitError::CacheSetupFailed)?,
        };

        let cache: Arc<dyn Cache> = Arc::new(c.with_io_log(options.io_log.clone()));

        let ignore_checksums =
            options.corrupt_section_policy == CorruptSectionPolicy::DamnTheTorpedoes;

        let io_log = options.io_log.clone();

        // read the segment metadata
        let segs = sp_itr
            .map(|p| p.as_ref().to_string())
            .collect::<Vec<_>>()
            //            .into_iter()
            .into_par_iter()
            .enumerate()
            .map(|(idx, sp)| {
                let io = make_bytes_reader(
                    &sp,
                    idx,
                    cache.clone(),
                    runtime.clone(),
                    s3_auth.as_ref(),
                    io_log.clone(),
                )?;
                read_segment(sp, idx, &io, ignore_checksums)
            })
            .collect::<Result<Vec<SegmentComponents>, _>>()?;

        // process segment metadata
        let meta = process_segments(segs)?;

        validate_volume(&meta.volume)?;

        let exp_chunk_count = meta.volume.chunk_count as usize;
        let chunk_count = meta.chunks.len();

        if chunk_count > exp_chunk_count {
            return Err(OpenError::TooManyChunks(chunk_count, exp_chunk_count));
        } else if chunk_count < exp_chunk_count {
            return Err(OpenError::TooFewChunks(chunk_count, exp_chunk_count));
        }

        let chunk_size = meta.volume.chunk_size();
        let sector_count = meta.volume.total_sector_count as usize;
        let sector_size = meta.volume.bytes_per_sector as usize;
        let image_size = meta.volume.max_offset() as u64;

        Ok(Self {
            segments: meta.segments,
            chunks: meta.chunks,
            chunk_count,
            chunk_size,
            sector_count,
            sector_size,
            image_size,
            stored_md5: meta.md5,
            stored_sha1: meta.sha1,
            segment_paths: meta.segment_paths,
            corrupt_section_policy: options.corrupt_section_policy,
            corrupt_chunk_policy: options.corrupt_chunk_policy,
            workers: vec![],
            cache,
            decoded_chunk_cache: Arc::new(Mutex::new(DecodedChunkCache::new(
                DECODED_CHUNK_CACHE_CHUNKS,
            ))),
            runtime,
            io_log: options.io_log.clone(),
            parallel_chunk_reads: options.parallel_chunk_reads,
            chunk_pool: build_chunk_pool(options.parallel_chunk_threads)?,
        })
    }

    pub fn read_at_offset(
        &mut self,
        mut offset: u64,
        mut buf: &mut [u8],
    ) -> Result<usize, ReadError> {
        let timer = self.io_log.as_ref().map(|_| ReadTimer::start());
        let read_offset = offset;
        // don't start reading past the end
        let image_end = self.image_size;
        if offset > image_end {
            return Err(ReadErrorKind::OffsetBeyondEnd(offset, image_end))?;
        }

        // limit the buffer to the image end
        if offset + buf.len() as u64 > image_end {
            buf = &mut buf[..(image_end - offset) as usize];
        }

        let buf_beg = offset;
        let buf_end = offset + buf.len() as u64;

        let chunk_size = self.chunk_size as u64;

        let beg_chunk_index = (buf_beg / chunk_size) as usize;
        let end_chunk_index = (buf_end / chunk_size + (buf_end % chunk_size).min(1)) as usize;

        let chunk_hit = if crate::readworker::ENABLE_DECODED_CHUNK_CACHE {
            let mut cache = self.decoded_chunk_cache.lock().unwrap();
            Some((beg_chunk_index..end_chunk_index).all(|idx| cache.get(idx).is_some()))
        } else {
            None
        };

        let foyer_trace = Arc::new(Mutex::new(ReadTrace::default()));

        // TODO: Number of workers should have some fixed/configured maximum,
        // should not scale with the number of chunks to be fetched.
        // resize_with, not resize: the latter eagerly builds a template ReadWorker
        // -- allocating a chunk_size+4 buffer -- on every call, even when no
        // resize is needed and the template is dropped unused.
        if end_chunk_index - beg_chunk_index > self.workers.len() {
            let (chunk_size, policy) = (self.chunk_size, self.corrupt_chunk_policy);
            self.workers
                .resize_with(end_chunk_index - beg_chunk_index, || {
                    ReadWorker::new(chunk_size, image_end, policy)
                });
        }

        let mut tasks = Vec::with_capacity(end_chunk_index - beg_chunk_index);
        let mut w = &mut self.workers[..];

        while offset < buf_end {
            // get the next chunk
            let chunk_index = (offset / chunk_size) as usize;

            let chunk = &self.chunks[chunk_index];
            let seg = &self.segments[chunk.segment];

            let chunk_beg = chunk_index as u64 * chunk_size;
            let chunk_end = std::cmp::min(chunk_beg + chunk_size, image_end);

            let beg_in_chunk = (offset - chunk_beg) as usize;
            let end_in_chunk = (std::cmp::min(chunk_end, buf_end) - chunk_beg) as usize;

            let beg_in_buf = offset - buf_beg;
            let end_in_buf = beg_in_buf + (end_in_chunk - beg_in_chunk) as u64;

            let (bleft, bright) = buf.split_at_mut((end_in_buf - beg_in_buf) as usize);
            buf = bright;

            let (wleft, wright) = w.split_at_mut(1);
            w = wright;

            let src = CacheWorkerSource {
                cache: self.cache.clone(),
                runtime: self.runtime.clone(),
                idx: chunk.segment,
                foyer_trace: Some(foyer_trace.clone()),
            };
            let decoded_chunk_cache = self.decoded_chunk_cache.clone();
            tasks.push((
                chunk_index,
                chunk,
                src,
                decoded_chunk_cache,
                bleft,
                beg_in_chunk,
                end_in_chunk,
                &seg.path,
                &mut wleft[0],
            ));

            offset += end_in_buf - beg_in_buf;
        }

        // A single chunk is driven inline either way; there is nothing to fan out.
        let fan_out = self.parallel_chunk_reads && tasks.len() > 1;

        if fan_out {
            match &self.chunk_pool {
                Some(pool) => {
                    pool.install(|| tasks.into_par_iter().try_for_each(run_chunk_task))?
                }
                None => tasks.into_par_iter().try_for_each(run_chunk_task)?,
            }
        } else {
            tasks.into_iter().try_for_each(run_chunk_task)?;
        }

        let read_len = (offset - buf_beg) as usize;
        if let Some(log) = &self.io_log {
            let dur_us = timer.as_ref().map(ReadTimer::elapsed_us).unwrap_or(0);
            let foyer = foyer_trace.lock().unwrap().foyer_label();
            log.log_read(
                read_offset,
                read_len,
                dur_us,
                foyer,
                chunk_cache_label(chunk_hit),
            );
        }

        Ok(read_len)
    }
}

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn repeated_partial_reads_match_single_read() {
        let options = E01ReaderOptions {
            corrupt_section_policy: CorruptSectionPolicy::Error,
            corrupt_chunk_policy: CorruptChunkPolicy::Error,
            foyer_readahead: 0,
            s3_concurrency: DEFAULT_S3_CONCURRENCY,
            cache_mem_mib: DEFAULT_CACHE_MEM_MIB,
            cache_mode: CacheMode::default(),
            cache_dir: None,
            io_log: None,
            parallel_chunk_reads: true,
            parallel_chunk_threads: DEFAULT_PARALLEL_CHUNK_THREADS,
        };
        let mut reader =
            E01Reader::open_glob(crate::test_data::IMAGE_E01.segment_paths[0], &options).unwrap();
        let chunk_size = reader.chunk_size as u64;
        let base = chunk_size * 3;
        let span = 4096usize;

        let mut first = vec![0u8; span];
        let mut second = vec![0u8; span];
        reader.read_at_offset(base + 100, &mut first).unwrap();
        reader
            .read_at_offset(base + 100 + span as u64, &mut second)
            .unwrap();

        let mut combined = vec![0u8; span * 2];
        reader.read_at_offset(base + 100, &mut combined).unwrap();

        assert_eq!(&combined[..span], &first[..]);
        assert_eq!(&combined[span..], &second[..]);
    }

    #[test]
    fn parallel_then_serial_reads_same_chunk_stay_consistent() {
        let options = E01ReaderOptions {
            corrupt_section_policy: CorruptSectionPolicy::Error,
            corrupt_chunk_policy: CorruptChunkPolicy::Error,
            foyer_readahead: 0,
            s3_concurrency: DEFAULT_S3_CONCURRENCY,
            cache_mem_mib: DEFAULT_CACHE_MEM_MIB,
            cache_mode: CacheMode::default(),
            cache_dir: None,
            io_log: None,
            parallel_chunk_reads: true,
            parallel_chunk_threads: DEFAULT_PARALLEL_CHUNK_THREADS,
        };
        let mut reader =
            E01Reader::open_glob(crate::test_data::IMAGE_E01.segment_paths[0], &options).unwrap();
        let chunk_size = reader.chunk_size as u64;
        let base = chunk_size * 2;

        let mut cross = vec![0u8; (chunk_size * 2) as usize];
        reader.read_at_offset(base, &mut cross).unwrap();

        let mut again = vec![0u8; 4096];
        reader.read_at_offset(base + 8192, &mut again).unwrap();

        assert_eq!(&again[..], &cross[8192..8192 + 4096]);
    }

    #[test]
    fn open_rejects_zero_volume_geometry() {
        // read_at_offset divides by chunk_size (sectors_per_chunk * bytes_per_sector),
        // so a volume section declaring either as zero must be rejected at open
        // rather than dividing by zero on the first read.
        for (sectors_per_chunk, bytes_per_sector) in [(0, 512), (64, 0), (0, 0)] {
            let volume = VolumeSection {
                chunk_count: 1,
                sectors_per_chunk,
                bytes_per_sector,
                total_sector_count: 1,
            };

            match validate_volume(&volume) {
                Err(OpenError::InvalidVolumeGeometry { .. }) => {}
                other => panic!("expected InvalidVolumeGeometry, got {other:?}"),
            }
        }
    }

    #[test]
    fn validate_volume_accepts_a_sane_geometry() {
        let volume = VolumeSection {
            chunk_count: 1,
            sectors_per_chunk: 64,
            bytes_per_sector: 512,
            total_sector_count: 64,
        };
        assert!(validate_volume(&volume).is_ok());
    }

    #[test]
    fn open_rejects_out_of_order_segments() {
        let options = E01ReaderOptions::default();
        let err = E01Reader::open(["data/mimage.E02", "data/mimage.E01"], &options).unwrap_err();
        match err {
            OpenError::SegmentOutOfOrder {
                actual, expected, ..
            } => {
                assert_eq!(actual, 2);
                assert_eq!(expected, 1);
            }
            e => panic!("expected SegmentOutOfOrder, got {e:?}"),
        }
    }
}
