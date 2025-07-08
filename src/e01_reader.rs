use std::path::{Path, PathBuf};
use tracing::{debug, debug_span, warn};

extern crate kaitai;

use kaitai::{BytesReader, KError};

use crate::error::{IoError, LibError};
use crate::sec_read::{VolumeSection, Section, SectionIterator};
use crate::seg_path::{find_segment_paths, UnrecognizedExtension};
use crate::segment::{Segment, SegmentFileHeader};

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
pub enum ReadError {
    #[error("Requested chunk number {0} does not exist")]
    BadChunkNumber(usize),
    #[error("Requested offset {0} is beyond end of image {1}")]
    OffsetBeyondEnd(usize, usize),
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

impl From<LibError> for ReadError {
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

impl ReadError {
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
pub enum E01Error {
    #[error("{0}")]
    OpenError(#[from] OpenError),
    #[error("{0}")]
    ReadError(#[from] ReadError)
}

#[derive(Debug)]
pub struct E01Reader {
    volume: VolumeSection,
    segments: Vec<(PathBuf, Segment)>,
    stored_md5: Option<Vec<u8>>,
    stored_sha1: Option<Vec<u8>>,
    ignore_checksums: bool
}

impl E01Reader {
    pub fn open_glob<T: AsRef<Path>>(
        example_segment_path: T,
        ignore_checksums: bool
    ) -> Result<Self, OpenError>
    {
        if example_segment_path.as_ref().exists() {
            Self::open(
                find_segment_paths(&example_segment_path)?,
                ignore_checksums
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
        ignore_checksums: bool
    ) -> Result<Self, OpenError>
    {
        let mut segment_paths = segment_paths.into_iter().peekable();

        // check that there are some segment files
        segment_paths.peek().ok_or(OpenError::NoSegmentFiles)?;

        let mut segments: Vec<(PathBuf, Segment)> = vec![];
        let mut chunk_count = 0;

        let mut volume = None;
        let mut stored_md5 = None;
        let mut stored_sha1 = None;
        let mut done = false;

        // read segments
        for sp in segment_paths.by_ref() {
            let _span = debug_span!("", segment_path = ?sp.as_ref()).entered();
            debug!("opening {}", sp.as_ref().display());

            let io = BytesReader::open(&sp)
                .map_err(OpenError::from)
                .map_err(|e| e.with_path(&sp))?;

            debug!("reading sections {}", sp.as_ref().display());

            let header = SegmentFileHeader::new(&io)
                .map_err(OpenError::from)
                .map_err(|e| e.with_path(&sp))?;

            let mut chunks = vec![];
            let mut end_of_sectors = 0;

            let mut seg_volume = None;
            let mut seg_stored_md5 = None;
            let mut seg_stored_sha1 = None;

            let mut sections = SectionIterator::new(&io, ignore_checksums);

            for section in sections.by_ref() {
                match section
                    .map_err(OpenError::from)
                    .map_err(|e| e.with_path(&sp))?
                {
                    Section::Volume(v) => seg_volume = Some(v),
                    Section::Table(t) => {
                        chunks.extend(t);
                        let chunks_len = chunks.len();
                        chunks[chunks_len - 1].end_offset = Some(end_of_sectors);
                    },
                    Section::Sectors(eos) => end_of_sectors = eos,
                    Section::Hash(h) => seg_stored_md5 = Some(h.md5().clone()),
                    Section::Digest(d) => {
                        seg_stored_md5 = Some(d.md5().clone());
                        seg_stored_sha1 = Some(d.sha1().clone());
                    },
                    Section::Done => { done = true; break; },
                    _ => {}
                }
            }

            if done && sections.next().is_some() {
                warn!("more sections after done");
            }

            let sp = sp.as_ref();

            // take the volume section if it's the first one
            match (seg_volume, &volume) {
                // we have no volume section, and saw one
                (Some(sv), None) => volume = Some(sv),
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
            match (seg_stored_md5, &stored_md5) {
                (Some(h), None) => stored_md5 = Some(h),
                (Some(new), Some(old)) if new != *old =>
                    warn!("duplicate stored MD5s disagree"),
                _ => {}
            }

            // take the stored SHA1 if it's the first one
            match (seg_stored_sha1, &stored_sha1) {
                (Some(h), None) => stored_sha1 = Some(h),
                (Some(new), Some(old)) if new != *old =>
                    warn!("duplicate stored SHA1s disagree"),
                _ => {}
            }

            let seg = Segment {
                io,
                _header: header,
                chunks,
                end_of_sectors
            };

            chunk_count += seg.chunk_count();
            segments.push((sp.into(), seg));

            if done {
                break;
            }
        }

        if done {
            if segment_paths.next().is_some() {
                warn!("more segments after finding done section");
            }
        }
        else {
            warn!("read all segments without finding done section");
        }

        let volume = volume.expect("volume section must have been found");

        let exp_chunk_count = volume.chunk_count as usize;

        if chunk_count > exp_chunk_count {
            return Err(OpenError::TooManyChunks(chunk_count, exp_chunk_count));
        }
        else if chunk_count < exp_chunk_count {
            return Err(OpenError::TooFewChunks(chunk_count, exp_chunk_count));
        }

        Ok(E01Reader {
            volume,
            segments,
            stored_md5,
            stored_sha1,
            ignore_checksums
        })
    }

    fn get_segment(
        &self,
        chunk_number: usize,
    ) -> Result<(&PathBuf, &Segment, usize), ReadError> {
        let mut chunks = 0;
        let mut chunk_index = 0;
// FIXME: Don't use an O(n) algorithm for locating chunks!
        self.segments
            .iter()
            .find(|(_, s)| {
                if chunk_number >= chunks && chunk_number < chunks + s.chunk_count() {
                    chunk_index = chunk_number - chunks;
                    return true;
                }
                chunks += s.chunk_count();
                false
            })
            .map(|(p, s)| (p, s, chunk_index))
            .ok_or(ReadError::BadChunkNumber(chunk_number))
    }

    pub fn read_at_offset(
        &self,
        mut offset: usize,
        buf: &mut [u8]
    ) -> Result<usize, ReadError>
    {
        let total_size = self.total_size();
        if offset > total_size {
            return Err(ReadError::OffsetBeyondEnd(offset, total_size));
        }

        let mut bytes_read = 0;
        let mut remaining_buf = &mut buf[..];

        while !remaining_buf.is_empty() && offset < total_size {
            let chunk_number = offset / self.chunk_size();
            debug_assert!(chunk_number < self.volume.chunk_count as usize);

            let (sp, seg, chunk_index) = self.get_segment(chunk_number)?;

            let mut data = seg.read_chunk(
                chunk_number,
                chunk_index,
                self.ignore_checksums,
                remaining_buf
            ).map_err(ReadError::from).map_err(|e| e.with_path(sp))?;

            if chunk_number * self.chunk_size() + data.len() > total_size {
                data.truncate(total_size - chunk_number * self.chunk_size());
            }

            let data_offset = offset % self.chunk_size();

            if buf.len() < bytes_read || data.len() < data_offset {
                println!("todo");
            }

            let remaining_size = std::cmp::min(buf.len() - bytes_read, data.len() - data_offset);
            remaining_buf = &mut buf[bytes_read..bytes_read + remaining_size];
            remaining_buf.copy_from_slice(&data[data_offset..data_offset + remaining_size]);

            debug_assert!(offset + remaining_size <= total_size);

            bytes_read += remaining_size;
            offset += remaining_size;
        }

        Ok(bytes_read)
    }

    pub fn chunk_size(&self) -> usize {
        self.volume.chunk_size()
    }

    pub fn total_size(&self) -> usize {
        self.volume.max_offset()
    }

    pub fn get_stored_md5(&self) -> Option<&[u8]> {
        self.stored_md5.as_deref()
    }

    pub fn get_stored_sha1(&self) -> Option<&[u8]> {
        self.stored_sha1.as_deref()
    }
}

#[cfg(test)]
mod test {
    use super::*;


}
