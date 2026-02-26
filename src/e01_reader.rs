use kaitai::{BytesReader, KError, ReadSeek};
use rayon::prelude::*;
use s3::{
    bucket::Bucket,
    creds::Credentials,
    region::Region
};
use std::{
    fmt::Debug,
    path::{Path, PathBuf},
    sync::{Arc, Mutex}
};
use tokio::runtime::Runtime;
use tracing::{debug, debug_span, trace, warn};
use url::{self, Url};

use crate::{
    bytessource::BytesSource,
    cache::Cache,
    cachereadseek::CacheReadSeek,
    cacheworkersource::CacheWorkerSource,
    dummycache::DummyCache,
    error::{IoError, LibError},
    foyercache::FoyerCache,
    filesource::FileSource,
    readworker::ReadWorker,
    s3source::S3Source,
    sec_read::{Chunk, VolumeSection, Section, SectionIterator},
    seg_path::{ExistsChecker, UnrecognizedExtension, validated_segment_paths},
    segment::SegmentFileHeader
};

#[derive(Debug, thiserror::Error)]
pub enum InitError {
    #[error("Failed to start tokio Runtime: {0}")]
    TokioRuntimeFailed(std::io::Error),
    #[error("{0}")]
    CacheSetupFailed(std::io::Error)
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
        source: LibError
    },
    #[error("Bad data in {path}: {source}")]
    BadData {
        path: String,
        #[source]
        source: LibError
    },
    #[error("Malformed path or URL: {0}")]
    BadPath(String),
    #[error("Unsupported URL scheme: {0}")]
    UnsupportedScheme(String),
    #[error("{0}")]
    InitializationFailed(#[from] InitError)
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
                source: e
            },
            _ => Self::BadData {
                path: "".into(), // set using with_path()
                source: e
            }
        }
    }
}

impl From<KError> for OpenError {
    fn from(e: KError) -> Self {
        Self::IoError {
            path: "".into(), // set using with_path()
            source: LibError::IoError(IoError::Read(e))
        }
    }
}

impl OpenError {
    fn with_path<T: AsRef<str>>(self, path: T) -> Self {
        match self {
            Self::IoError { source, .. } => Self::IoError {
                path: path.as_ref().into(),
                source
            },
            Self::BadData { source, .. } => Self::BadData {
                path: path.as_ref().into(),
                source
            },
            _ => self
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ReadErrorKind {
    #[error("Requested offset {0} is beyond end of image {1}")]
//    OffsetBeyondEnd(u64, u64),
    OffsetBeyondEnd(usize, usize),
    #[error("{0}")]
    IoError(#[from] std::io::Error),
    #[error("Chunk {0} is {1} bytes long, must be at least 5 bytes long")]
    TooShort(usize, usize),
    #[error("Chunk {0} checksum failed: calculated {1}, expected {2}")]
    BadChecksum(usize, u32, u32),
    #[error("Decompression of chunk {0} failed: {1}")]
    DecompressionFailed(usize, #[source] std::io::Error)
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
    source: ReadErrorKind
}

impl ReadError {
    fn with_path<T: AsRef<Path>>(self, path: T) -> Self {
        Self {
            path: Some(path.as_ref().into()),
            source: self.source
        }
    }
}

impl From<ReadErrorKind> for ReadError {
    fn from(e: ReadErrorKind) -> Self {
        Self {
            path: None,
            source: e
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum E01Error {
    #[error("{0}")]
    OpenError(#[from] OpenError),
    #[error("{0}")]
    ReadError(#[from] ReadError)
}

#[derive(Debug)]
struct Segment {
    pub path: String
}

struct SegmentComponents {
    path: String,
    volume: Option<VolumeSection>,
    md5: Option<[u8; 16]>,
    sha1: Option<[u8; 20]>,
    chunks: Vec<Chunk>,
    done: bool
}

fn read_segment<T: AsRef<str>>(
    segment_path: T,
    segment_index: usize,
    io: &BytesReader,
    ignore_checksums: bool
) -> Result<SegmentComponents, OpenError>
{
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
            },
            Section::Sectors(eos) => end_of_sectors = eos,
            Section::Hash(h) => md5 = Some(h),
            Section::Digest(d_md5, d_sha1) => {
                md5 = Some(d_md5);
                sha1 = Some(d_sha1);
            },
            Section::Done => { done = true; break; },
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

    Ok(
        SegmentComponents {
            path: segment_path.as_ref().into(),
            volume,
            md5,
            sha1,
            chunks,
            done
        }
    )
}

fn make_bytes_reader(
    p: &str,
    idx: usize,
    cache: Arc<Mutex<dyn Cache + Send>>,
    runtime: Arc<Runtime>
) -> Result<BytesReader, OpenError>
{
    debug!("opening {}", p);

    let url = path_or_url_to_url(p)
        .ok_or(OpenError::BadPath(p.into()))?;

    let src = source_for_url(&url, &runtime)?;

    let seg_len = src.end();
    cache.lock().unwrap().add_source(idx, src);

    let crs = CacheReadSeek::new(
        cache,
        runtime,
        idx,
        seg_len
    );

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
    chunks: Vec<Chunk>
}

fn process_segments<S: IntoIterator<Item = SegmentComponents>>(
    segs: S,
    ignore_checksums: bool
) -> Result<E01Metadata, OpenError>
{
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
                let unread_chunks = (sv.chunk_count as usize)
                    .saturating_sub(chunks.len());
                chunks.reserve_exact(unread_chunks);
                volume = Some(sv);
            },
            // we have a volume section, and didn't see a new one
            (None, Some(_)) => {},
            // we have no volume section, and saw none;
            // this can happen only on the first segment
            (None, None) =>
                return Err(OpenError::MissingVolumeSection((&seg.path).into())),
            // we have a volume section and saw another one!
            (Some(_), Some(_)) =>
                warn!("duplicate volume section")
        }

        // take the stored MD5 if it's the first one
        match (seg.md5, &stored_md5) {
            (Some(h), None) => stored_md5 = Some(h),
            (Some(new), Some(old)) if new != *old =>
                warn!("duplicate stored MD5s disagree"),
            _ => {}
        }

        // take the stored SHA1 if it's the first one
        match (seg.sha1, &stored_sha1) {
            (Some(h), None) => stored_sha1 = Some(h),
            (Some(new), Some(old)) if new != *old =>
                warn!("duplicate stored SHA1s disagree"),
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
            }
            else {
                done = true;
            }
        }
    }

    if !done {
        warn!("read all segments without finding done section");
    }

    let volume = volume.expect("volume section must have been found");

    Ok(
        E01Metadata {
            volume,
            md5: stored_md5,
            sha1: stored_sha1,
            segments,
            segment_paths,
            chunks
        }
    )
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub enum CorruptSectionPolicy {
    #[default]
    Error,
    DamnTheTorpedoes
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub enum CorruptChunkPolicy {
    Error,
    #[default]
    Zero,
    RawIfPossible
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct E01ReaderOptions {
    pub corrupt_section_policy: CorruptSectionPolicy,
    pub corrupt_chunk_policy: CorruptChunkPolicy
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
        r => r.ok()
    }
}

fn source_for_url(
    url: &Url,
    runtime: &Runtime
) -> Result<Box<dyn BytesSource + Send>, OpenError>
{
    match url.scheme() {
        "file" => {
            let p = if cfg!(windows) {
                // Windows file URLs get a spare / before the drive letter,
                // which we have to remove when using it as a path.
                url.path().trim_start_matches('/')
            }
            else {
                url.path()
            };

            let len = std::fs::metadata(p)
                .map_err(OpenError::from)
                .map_err(|e| e.with_path(p))?
                .len();
            Ok(Box::new(FileSource { path: p.into(), len }))
        },
        "s3" => {
            let name = url.host_str()
                .ok_or(OpenError::BadPath(url.to_string()))?;

            let bucket = *Bucket::new(
                name,
                Region::UsEast1,
                Credentials::anonymous().unwrap()
            )
            .map_err(std::io::Error::other)
            .map_err(OpenError::from)
            .map_err(|e| e.with_path(url))?;

            let key = url.path();

            let (h, _) = runtime.block_on(bucket.head_object(key))
                .map_err(std::io::Error::other)
                .map_err(OpenError::from)
                .map_err(|e| e.with_path(url))?;

            let len = h.content_length.unwrap().try_into().unwrap();
            debug!("content-length: {len}");

            Ok(Box::new(S3Source::new(bucket, key.into(), len)))
        },
        _ => Err(OpenError::UnsupportedScheme(url.to_string()))
    }
}

pub struct E01Reader {
    segments: Vec<Segment>,
    chunks: Vec<Chunk>,

    pub chunk_size: usize,
    pub chunk_count: usize,
    pub sector_size: usize,
    pub sector_count: usize,
    pub image_size: usize,

    pub stored_md5: Option<[u8; 16]>,
    pub stored_sha1: Option<[u8; 20]>,

    pub segment_paths: Vec<PathBuf>,

    corrupt_section_policy: CorruptSectionPolicy,
    corrupt_chunk_policy: CorruptChunkPolicy,

    workers: Vec<ReadWorker>,
    cache: Arc<Mutex<dyn Cache + Send>>,
    runtime: Arc<Runtime>
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
    bucket: Bucket,
    runtime: Arc<Runtime>
}

impl S3Checker {
    fn new(
        url: &Url,
        runtime: Arc<Runtime>
    ) -> Result<Self, OpenError> {
        let name = url.host_str()
            .ok_or(OpenError::BadPath(url.to_string()))?;

        let bucket = *Bucket::new(
            name,
            Region::UsEast1,
            Credentials::anonymous().unwrap()
        )
        .map_err(std::io::Error::other)
        .map_err(OpenError::from)
        .map_err(|e| e.with_path(url))?;

        Ok(Self { bucket, runtime })
    }
}

impl ExistsChecker for S3Checker {
    fn exists<T: AsRef<str>>(&mut self, path: T) -> bool {
        Url::parse(path.as_ref())
            .map(|url|
                self.runtime.block_on(self.bucket.head_object(url.path()))
                    .is_ok_and(|(_, code)| code == 200)
            )
            .unwrap_or(false)
    }
}

impl E01Reader {
    pub fn open_glob<T: AsRef<str>>(
        example_segment_path: T,
        options: &E01ReaderOptions
    ) -> Result<Self, OpenError>
    {
        let url = path_or_url_to_url(&example_segment_path)
            .ok_or(OpenError::BadPath(example_segment_path.as_ref().into()))?;

        let runtime = Arc::new(
            tokio::runtime::Runtime::new()
                .map_err(InitError::TokioRuntimeFailed)?
        );

        match url.scheme() {
            "file" => Self::open_impl(
                validated_segment_paths(
                    example_segment_path,
                    FileChecker,
                )?,
                options,
                runtime
            ),
            "s3" => Self::open_impl(
                validated_segment_paths(
                    example_segment_path,
                    S3Checker::new(&url, runtime.clone())?
                )?,
                options,
                runtime
            ),
            _ => Err(OpenError::UnsupportedScheme(url.to_string()))
        }
    }

    pub fn open<T: IntoIterator<Item: AsRef<str>>>(
        segment_paths: T,
        options: &E01ReaderOptions
    ) -> Result<Self, OpenError>
    {
        let runtime = Arc::new(
            tokio::runtime::Runtime::new()
                .map_err(InitError::TokioRuntimeFailed)?
        );

        Self::open_impl(segment_paths, options, runtime)
    }

    fn open_impl<T: IntoIterator<Item: AsRef<str>>>(
        segment_paths: T,
        options: &E01ReaderOptions,
        runtime: Arc<Runtime>
    ) -> Result<Self, OpenError>
    {
        let mut sp_itr = segment_paths.into_iter().peekable();

//        let c = DummyCache::new();

        let cache_disk_size = match sp_itr.peek() {
            Some(p) if p.as_ref().starts_with("s3://") => 256,
            Some(_) => 0,
            None => return Err(OpenError::NoSegmentFiles)
        };

        let cache_chunk_size = 1024 * 1024;
        let cache_mem_size = 1024;
        let c = runtime.block_on(
            FoyerCache::with_default_cache(
                cache_chunk_size,
                cache_mem_size,
                cache_disk_size,
                0
            )
        )
        .map_err(InitError::CacheSetupFailed)?;

        let cache = Arc::new(Mutex::new(c));

        let ignore_checksums = options.corrupt_section_policy == CorruptSectionPolicy::DamnTheTorpedoes;

        // read the segment metadata
        let segs = sp_itr.map(|p| p.as_ref().to_string())
            .collect::<Vec<_>>()
//            .into_iter()
            .into_par_iter()
            .enumerate()
            .map(|(idx, sp)| {
                let io = make_bytes_reader(
                    &sp,
                    idx,
                    cache.clone(),
                    runtime.clone()
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
        }
        else if chunk_count < exp_chunk_count {
            return Err(OpenError::TooFewChunks(chunk_count, exp_chunk_count));
        }

        let chunk_size = meta.volume.chunk_size();
        let sector_count = meta.volume.total_sector_count as usize;
        let sector_size = meta.volume.bytes_per_sector as usize;
        let image_size = meta.volume.max_offset();

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
            runtime
        })
    }

    pub fn read_at_offset(
        &mut self,
        mut offset: usize,
        mut buf: &mut [u8]
    ) -> Result<usize, ReadError>
    {
        // don't start reading past the end
        let image_end = self.image_size;
        if offset > image_end {
            return Err(ReadErrorKind::OffsetBeyondEnd(offset, image_end))?;
        }

        // limit the buffer to the image end
        if offset + buf.len() > image_end {
            buf = &mut buf[..(image_end - offset)];
        }

        let buf_beg = offset;
        let buf_end = offset + buf.len();

        let chunk_size = self.chunk_size;

        let beg_chunk_index = buf_beg / chunk_size;
        let end_chunk_index = buf_end / chunk_size + (buf_end % chunk_size).min(1);

// TODO: Number of workers should have some fixed/configured maximum,
// should not scale with the number of chunks to be fetched.
        if end_chunk_index - beg_chunk_index > self.workers.len() {
            self.workers.resize(
                end_chunk_index - beg_chunk_index,
                ReadWorker::new(
                    chunk_size,
                    image_end,
                    self.corrupt_chunk_policy
                )
            );
        }

        let mut tasks = Vec::with_capacity(end_chunk_index - beg_chunk_index);
        let mut w = &mut self.workers[..];

        while offset < buf_end {
            // get the next chunk
            let chunk_index = offset / chunk_size;

            let chunk = &self.chunks[chunk_index];
            let seg = &self.segments[chunk.segment];

            let chunk_beg = chunk_index * chunk_size;
            let chunk_end = std::cmp::min(chunk_beg + chunk_size, image_end);

            let beg_in_chunk = offset - chunk_beg;
            let end_in_chunk = std::cmp::min(chunk_end, buf_end) - chunk_beg;

            let beg_in_buf = offset - buf_beg;
            let end_in_buf = beg_in_buf + (end_in_chunk - beg_in_chunk);

            let (bleft, bright) = buf.split_at_mut(end_in_buf - beg_in_buf);
            buf = bright;

            let (wleft, wright) = w.split_at_mut(1);
            w = wright;

            let src = CacheWorkerSource {
                cache: self.cache.clone(),
                runtime: self.runtime.clone(),
                idx: chunk.segment
            };

            tasks.push((
                chunk_index,
                chunk,
                src,
                bleft,
                beg_in_chunk,
                end_in_chunk,
                &seg.path,
                &mut wleft[0]
            ));

            offset += end_in_buf - beg_in_buf;
        }

//        tasks.into_iter()
        tasks.into_par_iter()
            .try_for_each(|(chunk_index, chunk, mut src, sbuf, beg_in_chunk, end_in_chunk, seg_path, worker)| {
                worker.read(
                    chunk,
                    &mut src,
                    chunk_index,
                    sbuf,
                    beg_in_chunk,
                    end_in_chunk
                )
                .map_err(ReadError::from)
                .map_err(|e| e.with_path(seg_path))
            })?;

        Ok(offset - buf_beg)
    }
}

#[cfg(test)]
mod test {
}
