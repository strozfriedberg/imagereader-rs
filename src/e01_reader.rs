use std::path::{Path, PathBuf};

extern crate kaitai;
use self::kaitai::*;

//use crate::generated::ewf_section_descriptor_v2::*;
use crate::kerror_wrapper::FuckOffKError;
use crate::sec_read::{SectionError, VolumeSection};
use crate::seg_path::find_segment_paths;
use crate::segment::Segment;

#[derive(Debug, thiserror::Error)]
pub enum E01Error {
    #[error("Decompression failed")]
    DecompressionFailed(#[source] std::io::Error),
    #[error("Error while deserializing {name} struct: {source}")]
    DeserializationFailed {
        name: String,
        #[source]
        source: FuckOffKError
    },
    #[error("{source}")]
    OpenError {
        #[source]
        source: FuckOffKError
    },
    #[error("{source}")]
    ReadError {
        #[source]
        source: FuckOffKError
    },
    #[error("{source}")]
    SeekError {
        #[source]
        source: FuckOffKError
    },
    #[error("Segment file {file}, seek to {offset} failed: {source}")]
    SegmentSeekError {
        file: PathBuf,
        offset: usize,
        #[source]
        source: FuckOffKError
    },
    #[error("Unexpected volume size: {0}")]
    UnexpectedVolumeSize(u64),
    #[error("{0}")]
    IoError(#[from] std::io::Error),
    #[error("Invalid segment file")]
    InvalidSegmentFile,
    #[error("{0} checksum failed, calculated {1}, expected {2}")]
    BadChecksum(String, u32, u32),
    #[error("Unknown compression method value: {0}")]
    UnknownCompressionMethod(u16),
    #[error("Requested chunk number {0} is wrong")]
    BadChunkNumber(usize),
    #[error("Requested offset {0} is over max offset {1}")]
    OffsetBeyondEnd(usize, usize),
    #[error("Can't find file: {0}")]
    FileNotFound(PathBuf),
    #[error("Invalid EWF file: {0}")]
    InvalidFile(PathBuf),
    #[error("Invalid filename")]
    InvalidFilename,
    #[error("Invalid extension")]
    InvalidExtension,
    #[error("Missing volume section")]
    MissingVolumeSection,
    #[error("Missing some segment file")]
    MissingSegmentFile,
    #[error("Too many chunks")]
    TooManyChunks,
    #[error("Too few chunks")]
    TooFewChunks,
    #[error("Duplicate volume section")]
    DuplicateVolumeSection
}

impl From<SectionError> for E01Error {
    fn from(e: SectionError) -> E01Error {
        match e {
            SectionError::IoError(e) => E01Error::IoError(e),
            SectionError::ReadError(e) => E01Error::ReadError { source: e },
            SectionError::SeekError { offset, source } => E01Error::SeekError { source },
            SectionError::BadChecksum(s, a, e) => E01Error::BadChecksum(s, a, e),
            SectionError::DeserializationFailed { name, source } => E01Error::DeserializationFailed { name, source },
            SectionError::UnexpectedVolumeSize(s) => E01Error::UnexpectedVolumeSize(s)
        }
    }
}

#[derive(Debug)]
pub struct E01Reader {
    volume: VolumeSection,
    segments: Vec<Segment>,
    stored_md5: Option<Vec<u8>>,
    stored_sha1: Option<Vec<u8>>,
    ignore_checksums: bool
}

/*
#[derive(Debug, thiserror::Error)]
pub enum SegmentReadError {
    #[error("{}")]
    DeserializationFailed {
        name: String,
        #[source]
        source: FuckOffKError
    }
}
*/

// Errors should be: ioerror, bad paths, bad input

impl E01Reader {
    pub fn open_glob<T: AsRef<Path>>(
        example_segment_path: T,
        ignore_checksums: bool
    ) -> Result<Self, E01Error>
    {
        Self::open(
            find_segment_paths(&example_segment_path)
// TODO: report actual error
                .or(Err(E01Error::InvalidFilename))?,
            ignore_checksums
        )
    }

    pub fn open<T: IntoIterator<Item: AsRef<Path>>>(
        segment_paths: T,
        ignore_checksums: bool
    ) -> Result<Self, E01Error>
    {
        let mut segment_paths = segment_paths.into_iter();

        let mut volume_opt: Option<VolumeSection> = None;
        let mut stored_md5: Option<_> = None;
        let mut stored_sha1: Option<_> = None;

        let mut segments = vec![];
        let mut chunks = 0;

        // read first segment; volume section must be contained in it
        let sp = segment_paths.next()
            .ok_or(E01Error::InvalidFilename)?;

        let seg = Segment::read(
            sp,
            &mut volume_opt,
            &mut stored_md5,
            &mut stored_sha1,
            ignore_checksums,
        )?;

        let volume = volume_opt.ok_or(E01Error::MissingVolumeSection)?;
        let exp_chunks = volume.chunk_count as usize;

//        let mut stored_md5_unexpected = None;
//        let mut stored_sha1_unexpected = None;
        volume_opt = None;

        chunks += seg.chunk_count();
        segments.push(seg);

        // continue reading segments
        for sp in segment_paths {
            let seg = Segment::read(
                sp,
                &mut volume_opt,
//                &mut stored_md5_unexpected,
//                &mut stored_sha1_unexpected,
                &mut stored_md5,
                &mut stored_sha1,
                ignore_checksums,
            )?;

            // we should not see volume, hash, digest sections again
            if volume_opt.is_some() {
                return Err(E01Error::DuplicateVolumeSection);
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
            segments.push(seg);
        }

        if chunks > exp_chunks {
            return Err(E01Error::TooManyChunks);
        }
        else if chunks < exp_chunks {
            return Err(E01Error::TooFewChunks);
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
    ) -> Result<&Segment, E01Error> {
        let mut chunks = 0;
        self.segments
            .iter()
            .find(|s| {
                if chunk_number >= chunks && chunk_number < chunks + s.chunk_count() {
                    *chunk_index = chunk_number - chunks;
                    return true;
                }
                chunks += s.chunk_count();
                false
            })
            .ok_or(E01Error::BadChunkNumber(chunk_number))
    }

    pub fn read_at_offset(
        &self,
        mut offset: usize,
        buf: &mut [u8]
    ) -> Result<usize, E01Error>
    {
        let total_size = self.total_size();
        if offset > total_size {
            return Err(E01Error::OffsetBeyondEnd(offset, total_size));
        }

        let mut bytes_read = 0;
        let mut remaining_buf = &mut buf[..];

        while !remaining_buf.is_empty() && offset < total_size {
            let chunk_number = offset / self.chunk_size();
            debug_assert!(chunk_number < self.volume.chunk_count as usize);
            let mut chunk_index = 0;

            let mut data = self
                .get_segment(chunk_number, &mut chunk_index)?
                .read_chunk(
                    chunk_number,
                    chunk_index,
                    self.ignore_checksums,
                    remaining_buf
                )?;

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
