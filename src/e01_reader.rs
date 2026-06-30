use kaitai::{BytesReader, KError, ReadSeek};
use rayon::prelude::*;
use s3::{bucket::Bucket, region::Region};
use std::{
    fmt::Debug,
    path::{Path, PathBuf},
    str::FromStr,
    sync::{Arc, Mutex, atomic::AtomicBool},
};
use tokio::runtime::Runtime;
use tracing::{debug, debug_span, trace, warn};
use url::{self, Url};

use crate::io_log::{IoLog, ReadTimer, ReadTrace, chunk_cache_label};
use crate::s3_creds::{S3Auth, resolve_s3_auth, s3_region_name, snapshot_credentials_sync};
use crate::{
    bytessource::BytesSource,
    cache::Cache,
    cachereadseek::CacheReadSeek,
    cacheworkersource::CacheWorkerSource,
    dummycache::DummyCache,
    error::{IoError, LibError},
    filesource::FileSource,
    foyercache::FoyerCache,
    readworker::{DecodedChunkCache, ReadWorker},
    s3source::S3Source,
    sec_read::{Chunk, Section, SectionIterator, VolumeSection},
    seg_path::{ExistsChecker, UnrecognizedExtension, validated_segment_paths},
    segment::SegmentFileHeader,
};

#[derive(Debug, thiserror::Error)]
pub enum InitError {
    #[error("Failed to start tokio Runtime: {0}")]
    TokioRuntimeFailed(std::io::Error),
    #[error("{0}")]
    CacheSetupFailed(std::io::Error),
}

#[derive(Debug, thiserror::Error)]
pub enum OpenError {
    #[error("{0}")]
    PathGlobError(#[from] UnrecognizedExtension),
    #[error("No segment files given")]
    NoSegmentFiles,
    #[error("Missing volume section in {0}")]
    MissingVolumeSection(PathBuf),
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

    let _header = SegmentFileHeader::new(io)
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
            Section::Table(t) => {
                if !t.is_empty() {
                    chunks.extend(t);
                    // set the end of the last chunk in the table
                    let chunks_len = chunks.len();
                    chunks[chunks_len - 1].end_offset = end_of_sectors;
                }
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
    cache: Arc<Mutex<dyn Cache + Send>>,
    runtime: Arc<Runtime>,
    s3_auth: Option<&Arc<S3Auth>>,
    io_log: Option<Arc<IoLog>>,
) -> Result<BytesReader, OpenError> {
    debug!("opening {}", p);

    let url = path_or_url_to_url(p).ok_or(OpenError::BadPath(p.into()))?;

    let src = source_for_url(&url, idx, &runtime, s3_auth, io_log.as_ref())?;

    let seg_len = src.end();
    cache.lock().unwrap().add_source(idx, src);

    let crs = CacheReadSeek::new(cache, runtime, idx, seg_len);

    let rs = Box::new(crs) as Box<dyn ReadSeek>;

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
    ignore_checksums: bool,
) -> Result<E01Metadata, OpenError> {
    let mut volume = None;
    let mut stored_md5 = None;
    let mut stored_sha1 = None;

    let mut segments = vec![];
    let mut segment_paths = vec![];
    let mut chunks = vec![];

    let mut done = false;

    for seg in segs {
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

/// Default foyer memory cache capacity (~1 MiB entries when chunk size is 1 MiB).
pub const DEFAULT_CACHE_MEM_MIB: usize = 1024;

/// How the foyer cache is structured for a session.
#[derive(Debug, Clone)]
pub enum CacheMode {
    /// Local-file backing: a single memory-only cache.
    SingleMemory,
    /// S3 backing: a dedicated metadata cache plus a content cache. `regular_phase`
    /// starts `false` (metadata phase) and is flipped to `true` by the SIGUSR1 handler.
    DualHybrid {
        content_disk_mib: usize,
        metadata_mem_mib: usize,
        metadata_disk_mib: usize,
        regular_phase: Arc<AtomicBool>,
    },
}

impl Default for CacheMode {
    fn default() -> Self {
        CacheMode::SingleMemory
    }
}

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
    /// When set, append JSONL read/S3 traces (see [`IoLog`]).
    pub io_log: Option<Arc<IoLog>>,
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
            io_log: None,
        }
    }
}

fn path_or_url_to_url<P: AsRef<str>>(p: P) -> Option<Url> {
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

/// Whether a region was explicitly resolved from env vars or the AWS profile.
/// When false (and the host is not an access-point alias), `s3_bucket` discovers
/// the bucket's real region via GetBucketLocation rather than assuming one.
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

fn s3_bucket(
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

fn source_for_url(
    url: &Url,
    segment: usize,
    runtime: &Runtime,
    s3_auth: Option<&Arc<S3Auth>>,
    io_log: Option<&Arc<IoLog>>,
) -> Result<Box<dyn BytesSource + Send + Sync>, OpenError> {
    match url.scheme() {
        "file" => {
            let p = if cfg!(windows) {
                // Windows file URLs get a spare / before the drive letter,
                // which we have to remove when using it as a path.
                url.path().trim_start_matches('/')
            } else {
                url.path()
            };

            let len = std::fs::metadata(p)
                .map_err(OpenError::from)
                .map_err(|e| e.with_path(p))?
                .len();
            Ok(Box::new(FileSource {
                path: p.into(),
                len,
            }))
        }
        "s3" => {
            let auth = s3_auth.ok_or_else(|| {
                OpenError::from(std::io::Error::other("S3 credentials not resolved"))
            })?;
            let name = url.host_str().ok_or(OpenError::BadPath(url.to_string()))?;
            let key = url.path().trim_start_matches('/');
            let bucket = s3_bucket(name, url.as_ref(), runtime, auth)?;

            let (h, _) = runtime
                .block_on(bucket.head_object(key))
                .map_err(std::io::Error::other)
                .map_err(OpenError::from)
                .map_err(|e| e.with_path(url))?;

            let len = h.content_length.unwrap().try_into().unwrap();
            debug!("content-length: {len}");

            Ok(Box::new(S3Source::new(
                bucket,
                key.to_string(),
                len,
                auth.clone(),
                segment,
                io_log.cloned(),
            )))
        }
        _ => Err(OpenError::UnsupportedScheme(url.to_string())),
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
    cache: Arc<Mutex<dyn Cache + Send>>,
    decoded_chunk_cache: Arc<Mutex<DecodedChunkCache>>,
    runtime: Arc<Runtime>,
    io_log: Option<Arc<IoLog>>,
}

const DECODED_CHUNK_CACHE_CHUNKS: usize = 1024;

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
                ))
                .map_err(InitError::CacheSetupFailed)?,
        };

        let cache = Arc::new(Mutex::new(c));

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
        let meta = process_segments(segs, ignore_checksums)?;

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
        if end_chunk_index - beg_chunk_index > self.workers.len() {
            self.workers.resize(
                end_chunk_index - beg_chunk_index,
                ReadWorker::new(self.chunk_size, image_end, self.corrupt_chunk_policy),
            );
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

        if tasks.len() == 1 {
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
            ) = tasks.into_iter().next().expect("one task");
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
                .map_err(|e| e.with_path(seg_path))?;
        } else {
            tasks.into_par_iter().try_for_each(
                |(
                    chunk_index,
                    chunk,
                    mut src,
                    decoded_chunk_cache,
                    sbuf,
                    beg_in_chunk,
                    end_in_chunk,
                    seg_path,
                    worker,
                )| {
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
                },
            )?;
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
    use s3::creds::Credentials;

    #[test]
    fn repeated_partial_reads_match_single_read() {
        let options = E01ReaderOptions {
            corrupt_section_policy: CorruptSectionPolicy::Error,
            corrupt_chunk_policy: CorruptChunkPolicy::Error,
            foyer_readahead: 0,
            s3_concurrency: DEFAULT_S3_CONCURRENCY,
            cache_mem_mib: DEFAULT_CACHE_MEM_MIB,
            cache_mode: CacheMode::default(),
            io_log: None,
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
            io_log: None,
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
