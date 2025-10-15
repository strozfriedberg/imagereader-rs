use std::{
    fs::File,
    path::{Path, PathBuf}
};
use tracing::{debug, debug_span, warn};

extern crate kaitai;

use kaitai::{BytesReader, KError};

use crate::error::{IoError, LibError};
use crate::readworker::ReadWorker;
use crate::sec_read::{Chunk, VolumeSection, Section, SectionIterator};
use crate::seg_path::{find_segment_paths, UnrecognizedExtension};
use crate::segment::SegmentFileHeader;

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
        path: PathBuf,
        #[source]
        source: LibError
    },
    #[error("Bad data in {path}: {source}")]
    BadData {
        path: PathBuf,
        #[source]
        source: LibError
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
            source: LibError::IoError(IoError::ReadError(e))
        }
    }
}

impl OpenError {
    fn with_path<T: AsRef<Path>>(self, path: T) -> Self {
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
    pub path: PathBuf
}

struct SegmentComponents {
    volume: Option<VolumeSection>,
    md5: Option<[u8; 16]>,
    sha1: Option<[u8; 20]>,
    chunks: Vec<Chunk>,
    done: bool
}

fn read_segment<T: AsRef<Path>>(
    segment_path: T,
    segment_index: usize,
    io: &BytesReader,
    ignore_checksums: bool
) -> Result<SegmentComponents, OpenError>
{
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
        match section
            .map_err(OpenError::from)
            .map_err(|e| e.with_path(&segment_path))?
        {
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
            volume,
            md5,
            sha1,
            chunks,
            done
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

#[derive(Debug)]
pub struct E01Reader {
    segments: Vec<Segment>,
    chunks: Vec<Chunk>,

    pub chunk_size: usize,
    pub chunk_count: usize,
    pub sector_count: usize,
    pub sector_size: usize,
    pub image_size: usize,

    pub stored_md5: Option<[u8; 16]>,
    pub stored_sha1: Option<[u8; 20]>,

    pub segment_paths: Vec<PathBuf>,

    corrupt_section_policy: CorruptSectionPolicy,
    corrupt_chunk_policy: CorruptChunkPolicy,

    workers: Vec<ReadWorker>
}

impl E01Reader {
    pub fn open_glob<T: AsRef<Path>>(
        example_segment_path: T,
        options: &E01ReaderOptions
    ) -> Result<Self, OpenError>
    {
        if example_segment_path.as_ref().exists() {
            Self::open(
                find_segment_paths(&example_segment_path)?,
                options
            )
        }
        else {
            Err(
                OpenError::IoError {
                    path: example_segment_path.as_ref().into(),
                    source: LibError::IoError(
                        IoError::IoError(
                            std::io::ErrorKind::NotFound.into()
                        )
                    )
                }
            )
        }
    }

    pub fn open<T: IntoIterator<Item: AsRef<Path>>>(
        segment_paths: T,
        options: &E01ReaderOptions
    ) -> Result<Self, OpenError>
    {
        let mut sp_itr = segment_paths.into_iter().peekable();

        // check that there are some segment files
        sp_itr.peek().ok_or(OpenError::NoSegmentFiles)?;

        let mut volume = None;
        let mut stored_md5 = None;
        let mut stored_sha1 = None;

        let mut segments = vec![];
        let mut segment_paths = vec![];
        let mut chunks = vec![];

        let mut done = false;

        let ignore_checksums = options.corrupt_section_policy == CorruptSectionPolicy::DamnTheTorpedoes;

        // read segments
        for sp in sp_itr.by_ref() {
            let sp = sp.as_ref();

            let _span = debug_span!("", segment_path = ?sp).entered();
            debug!("opening {}", sp.display());

            let io = BytesReader::open(sp)
                .map_err(OpenError::from)
                .map_err(|e| e.with_path(sp))?;

            debug!("reading sections {}", sp.display());

            let seg = read_segment(sp, segments.len(), &io, ignore_checksums)?;

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
                    return Err(OpenError::MissingVolumeSection(sp.into())),
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
            segments.push(Segment {
                path: sp.into()
            });

            segment_paths.push(sp.into());

            if seg.done {
                done = true;
                break;
            }
        }

        if done {
            if sp_itr.next().is_some() {
                warn!("more segments after finding done section");
            }
        }
        else {
            warn!("read all segments without finding done section");
        }

        let volume = volume.expect("volume section must have been found");

        let exp_chunk_count = volume.chunk_count as usize;
        let chunk_count = chunks.len();

        if chunk_count > exp_chunk_count {
            return Err(OpenError::TooManyChunks(chunk_count, exp_chunk_count));
        }
        else if chunk_count < exp_chunk_count {
            return Err(OpenError::TooFewChunks(chunk_count, exp_chunk_count));
        }

        let chunk_size = volume.chunk_size();
        let sector_count = volume.total_sector_count as usize;
        let sector_size = volume.bytes_per_sector as usize;
        let image_size = volume.max_offset();

        Ok(E01Reader {
            segments,
            chunks,
            chunk_count,
            chunk_size,
            sector_count,
            sector_size,
            image_size,
            stored_md5,
            stored_sha1,
            segment_paths,
            corrupt_section_policy: options.corrupt_section_policy,
            corrupt_chunk_policy: options.corrupt_chunk_policy,
            workers: vec![]
        })
    }

    pub fn read_at_offset(
        &mut self,
        mut offset: usize,
        mut buf: &mut [u8]
    ) -> Result<usize, ReadError>
    {
//        use rayon::prelude::*;

        // don't start reading past the end
        let image_end = self.image_size;
        if offset > image_end {
            return Err(ReadErrorKind::OffsetBeyondEnd(offset, image_end))?;
        }

        // clamp the buffer to the end
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

            let handle = File::open(&seg.path)
                .map_err(ReadErrorKind::IoError)?;

            tasks.push((
                chunk_index,
                chunk,
                handle,
                bleft,
                beg_in_chunk,
                end_in_chunk,
                &seg.path,
                &mut wleft[0]
            ));

            offset += end_in_buf - beg_in_buf;
        }

        tasks.into_iter()
//        tasks.into_par_iter()
            .try_for_each(|(chunk_index, chunk, mut handle, sbuf, beg_in_chunk, end_in_chunk, seg_path, worker)| {
                worker.read(
                    chunk,
                    &mut handle,
                    chunk_index,
                    sbuf,
                    beg_in_chunk,
                    end_in_chunk
                )
                .map_err(ReadError::from)
                .map_err(|e| e.with_path(seg_path))
                .and(Ok(()))
            })?;

        Ok(offset - buf_beg)
    }
}

#[cfg(test)]
mod test {
    use super::*;


}
