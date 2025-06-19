use std::path::{Path, PathBuf};

extern crate kaitai;

use kaitai::{BytesReader, KError};

use crate::error::{IoError, LibError};
use crate::sec_read::VolumeSection;
use crate::seg_path::{find_segment_paths, SegmentPathError};
use crate::segment::Segment;

#[derive(Debug, thiserror::Error)]
pub enum OpenError {
    #[error("{0}")]
    PathGlobError(#[from] SegmentPathError),
    #[error("No segment files given")]
    NoSegmentFiles,
    #[error("Missing volume section in {0}")]
    MissingVolumeSection(PathBuf),
    #[error("Duplicate volume section in {0}")]
    DuplicateVolumeSection(PathBuf),
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
                path: "".into(),
                source: e
            },
            _ => Self::BadData {
                path: "".into(),
                source: e
            }
        }
    }
}

impl From<KError> for OpenError {
    fn from(e: KError) -> Self {
        Self::IoError {
            path: "".into(),
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
                path: "".into(),
                source: e
            },
            _ => Self::BadData {
                path: "".into(),
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
        Self::open(
            find_segment_paths(&example_segment_path)?,
            ignore_checksums
        )
    }

    pub fn open<T: IntoIterator<Item: AsRef<Path>>>(
        segment_paths: T,
        ignore_checksums: bool
    ) -> Result<Self, OpenError>
    {
        let mut segment_paths = segment_paths.into_iter();

        let mut volume_opt: Option<VolumeSection> = None;
        let mut stored_md5: Option<_> = None;
        let mut stored_sha1: Option<_> = None;

        let mut segments = vec![];
        let mut chunks = 0;

        // read first segment; volume section must be contained in it
        let sp = segment_paths.next().ok_or(OpenError::NoSegmentFiles)?;

        let io = BytesReader::open(&sp)
            .map_err(OpenError::from)
            .map_err(|e| e.with_path(&sp))?;

        let seg = Segment::read(
            io,
            &mut volume_opt,
            &mut stored_md5,
            &mut stored_sha1,
            ignore_checksums,
        ).map_err(OpenError::from).map_err(|e| e.with_path(&sp))?;

        let volume = volume_opt
            .ok_or(OpenError::MissingVolumeSection(sp.as_ref().into()))?;
        let exp_chunks = volume.chunk_count as usize;

//        let mut stored_md5_unexpected = None;
//        let mut stored_sha1_unexpected = None;
        volume_opt = None;

        chunks += seg.chunk_count();
        segments.push((sp.as_ref().into(), seg));

        // continue reading segments
        for sp in segment_paths {
            let io = BytesReader::open(&sp)
                .map_err(OpenError::from)
                .map_err(|e| e.with_path(&sp))?;

            let seg = Segment::read(
                io,
                &mut volume_opt,
//                &mut stored_md5_unexpected,
//                &mut stored_sha1_unexpected,
                &mut stored_md5,
                &mut stored_sha1,
                ignore_checksums
            ).map_err(OpenError::from).map_err(|e| e.with_path(&sp))?;

            // we should not see volume, hash, digest sections again
            if volume_opt.is_some() {
                return Err(OpenError::DuplicateVolumeSection(sp.as_ref().into()));
            }

/*
            if stored_md5_unexpected.is_some() {
                return Err(E01Error::DuplicateMD5);
            }

            if stored_sha1_unexpected.is_some() {
                return Err(E01Error::DuplicateSHA1);
            }
*/

            chunks += seg.chunk_count();
            segments.push((sp.as_ref().into(), seg));
        }

        if chunks > exp_chunks {
            return Err(OpenError::TooManyChunks(chunks, exp_chunks));
        }
        else if chunks < exp_chunks {
            return Err(OpenError::TooFewChunks(chunks, exp_chunks));
        }

/*
        let segment_paths = candidate_segments(&f)
            .ok_or(E01Error::InvalidFilename)?;

        for sp in segment_paths {
            let seg = Segment::read(
                sp,
                &mut volume_opt,
                &mut stored_md5,
                &mut stored_sha1,
                ignore_checksums,
            )?;

            chunks += seg.chunks.len();
            segments.push(seg);

            if let Some(ref volume) = volume_opt {
                let exp_chunks = volume.chunk_count as usize;
                if chunks == exp_chunks {
                    break;
                }
                else if chunks > exp_chunks {
                    return Err(E01Error::TooManyChunks)
                }
            }
        }
*/

        Ok(E01Reader {
            volume,
            segments,
            stored_md5,
            stored_sha1,
            ignore_checksums
        })
    }

    pub fn total_size(&self) -> usize {
        self.volume.max_offset()
    }

    fn get_segment(
        &self,
        chunk_number: usize,
        chunk_index: &mut usize,
    ) -> Result<&(PathBuf, Segment), ReadError> {
        let mut chunks = 0;
// FIXME: Don't use an O(n) algorithm for locating chunks!
        self.segments
            .iter()
            .find(|(_, s)| {
                if chunk_number >= chunks && chunk_number < chunks + s.chunk_count() {
                    *chunk_index = chunk_number - chunks;
                    return true;
                }
                chunks += s.chunk_count();
                false
            })
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

            let mut chunk_index = 0;
            let (sp, seg) = self.get_segment(chunk_number, &mut chunk_index)?;

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

    pub fn get_stored_md5(&self) -> Option<&Vec<u8>> {
        self.stored_md5.as_ref()
    }

    pub fn get_stored_sha1(&self) -> Option<&Vec<u8>> {
        self.stored_sha1.as_ref()
    }
}

#[cfg(test)]
mod test {
    use super::*;


}
