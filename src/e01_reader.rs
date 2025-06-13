use flate2::read::ZlibDecoder;
use std::{
    convert::TryFrom,
    io::Read
};
use std::path::{Path, PathBuf};

extern crate kaitai;
use self::kaitai::*;

use crate::generated::ewf_file_header_v1::*;
use crate::generated::ewf_file_header_v2::*;
use crate::generated::ewf_section_descriptor_v1::*;
//use crate::generated::ewf_section_descriptor_v2::*;
use crate::generated::ewf_digest_section::*;
use crate::generated::ewf_hash_section::*;
use crate::generated::ewf_table_header::*;
use crate::generated::ewf_volume::*;
use crate::generated::ewf_volume_smart::*;

use crate::seg_path::{FileGlobber, find_segment_paths};

#[derive(Debug)]
pub struct FuckOffKError(KError);

impl std::fmt::Display for FuckOffKError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:?}", self.0)
    }
}

impl std::error::Error for FuckOffKError {}

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
    #[error("Unknown volume size: {0}")]
    UnknownVolumeSize(u64),
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
    #[error("Glob error")]
    GlobError,
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

#[derive(Debug)]
pub enum CompressionMethod {
    None = 0,
    Deflate = 1,
    Bzip = 2,
}

impl TryFrom<u16> for CompressionMethod {
    type Error = E01Error;

    fn try_from(v: u16) -> Result<Self, Self::Error> {
        match v {
            x if x == Self::None as u16 => Ok(Self::None),
            x if x == Self::Deflate as u16 => Ok(Self::Deflate),
            x if x == Self::Bzip as u16 => Ok(Self::Bzip),
            _ => Err(E01Error::UnknownCompressionMethod(v))
        }
    }
}

#[derive(Debug)]
pub struct SegmentFileHeader {
    pub major_version: u8,
    pub minor_version: u8,
    pub compr_method: CompressionMethod,
    pub segment_number: u16,
}

impl SegmentFileHeader {
    pub fn new(io: &BytesReader) -> Result<Self, E01Error> {
        let first_bytes = io
            .read_bytes(8)
            .map_err(|e| E01Error::ReadError { source: FuckOffKError(e) })?;

        io.seek(0)
            .map_err(|e| E01Error::SeekError { source: FuckOffKError(e) })?;

        // read file header

        if first_bytes == [0x45, 0x56, 0x46, 0x09, 0x0d, 0x0a, 0xff, 0x00] // EWF, EWF-E01, EWF-S01
            || first_bytes == [0x4c, 0x56, 0x46, 0x09, 0x0d, 0x0a, 0xff, 0x00]
        // EWF-L01
        // V1
        {
            match EwfFileHeaderV1::read_into::<_, EwfFileHeaderV1>(io, None, None) {
                Ok(h) => {
                    Ok(SegmentFileHeader {
                        major_version: 1,
                        minor_version: 0,
                        compr_method: CompressionMethod::Deflate,
                        segment_number: *h.segment_number(),
                    })
                }
                Err(e) => {
                    Err(E01Error::DeserializationFailed { name: "EwfFileHeaderV1".into(), source: FuckOffKError(e) })
                }
            }
        } else if first_bytes == [0x45, 0x56, 0x46, 0x32, 0x0d, 0x0a, 0x81, 0x00] // EVF2
            || first_bytes == [0x4c, 0x45, 0x46, 0x32, 0x0d, 0x0a, 0x81, 0x00]
        // LEF2
        // V2
        {
            match EwfFileHeaderV2::read_into::<_, EwfFileHeaderV2>(io, None, None) {
                Ok(h) => {
                    Ok(SegmentFileHeader {
                        major_version: *h.major_version(),
                        minor_version: *h.minor_version(),
                        compr_method: (*h.compression_method()).try_into()?,
                        segment_number: *h.segment_number(),
                    })
                }
                Err(e) => {
                    Err(E01Error::DeserializationFailed { name: "EwfFileHeaderV2".into(), source: FuckOffKError(e) })
                }
            }
        }
        else {
            Err(E01Error::InvalidSegmentFile)
        }
    }
}

fn checksum(bytes: &[u8]) -> Result<u32, E01Error> {
    Ok(adler32::adler32(std::io::Cursor::new(bytes))?)
}

fn checksum_reader(
    reader: &BytesReader,
    len: usize
) -> Result<u32, E01Error>
{
    checksum(
        &reader
            .read_bytes(len)
            .map_err(|e| E01Error::ReadError { source: FuckOffKError(e) })?
    )
}

fn checksum_ok(
    section_type: &str,
    io: &BytesReader,
    section_io: &BytesReader,
    crc_stored: u32,
) -> Result<(), E01Error>
{
    let crc = checksum_reader(section_io, io.pos() - section_io.pos() - 4)?;
    match crc == crc_stored {
        true => Ok(()),
        false => Err(E01Error::BadChecksum(section_type.into(), crc, crc_stored))
    }
}

#[derive(Debug, Default)]
pub struct VolumeSection {
    pub chunk_count: u32,
    pub sector_per_chunk: u32,
    pub bytes_per_sector: u32,
    pub total_sector_count: u64
}

impl VolumeSection {
    pub fn new(io: &BytesReader, size: u64, ignore_checksums: bool) -> Result<Self, E01Error> {
        // read volume section
        if size == 1052 {
            let vol_section =
                EwfVolume::read_into::<_, EwfVolume>(io, None, None)
                    .map_err(|e| E01Error::DeserializationFailed { name: "EwfVolume".into(), source: FuckOffKError(e) })?;

            if !ignore_checksums {
                checksum_ok(
                    "Volume section",
                    io,
                    &vol_section._io(),
                    *vol_section.checksum(),
                )?;
            }

            let vs = VolumeSection {
                chunk_count: *vol_section.number_of_chunks(),
                sector_per_chunk: *vol_section.sector_per_chunk(),
                bytes_per_sector: *vol_section.bytes_per_sector(),
                total_sector_count: *vol_section.number_of_sectors(),
            };
            Ok(vs)
        }
        else if size == 94 {
            let vol_section = EwfVolumeSmart::read_into::<_, EwfVolumeSmart>(io, None, None)
                .map_err(|e| E01Error::DeserializationFailed { name: "EwfVolumeSmart".into(), source: FuckOffKError(e) })?;

            if !ignore_checksums {
                checksum_ok(
                    "Volume section",
                    io,
                    &vol_section._io(),
                    *vol_section.checksum(),
                )?;
            }

            let vs = VolumeSection {
                chunk_count: *vol_section.number_of_chunks(),
                sector_per_chunk: *vol_section.sector_per_chunk(),
                bytes_per_sector: *vol_section.bytes_per_sector(),
                total_sector_count: *vol_section.number_of_sectors() as u64,
            };
            Ok(vs)
        }
        else {
            Err(E01Error::UnknownVolumeSize(size))
        }
    }

    pub fn chunk_size(&self) -> usize {
        self.sector_per_chunk as usize * self.bytes_per_sector as usize
    }

    pub fn max_offset(&self) -> usize {
        self.total_sector_count as usize * self.bytes_per_sector as usize
    }
}

#[derive(Debug)]
pub struct E01Reader {
    volume: VolumeSection,
    segments: Vec<Segment>,
    ignore_checksums: bool,
    stored_md5: Option<Vec<u8>>,
    stored_sha1: Option<Vec<u8>>,
}

#[derive(Debug)]
pub struct Segment {
    io: BytesReader,
    _header: SegmentFileHeader,
    chunks: Vec<Chunk>,
    end_of_sectors: u64,
}

#[derive(Debug)]
struct Chunk {
    data_offset: u64,
    compressed: bool,
    end_offset: Option<u64>,
}

impl Segment {
    fn read_table(
        io: &BytesReader,
        _size: u64,
        ignore_checksums: bool,
    ) -> Result<Vec<Chunk>, E01Error> {
        let table_section = EwfTableHeader::read_into::<_, EwfTableHeader>(io, None, None)
            .map_err(|e| E01Error::DeserializationFailed { name: "EwfTableHeader".into(), source: FuckOffKError(e) })?;

        if !ignore_checksums {
            checksum_ok(
                "Table section",
                io,
                &table_section._io(),
                *table_section.checksum(),
            )?;
        }

        let io_offsets = Clone::clone(io);
        let mut data_offset: u64;
        let mut chunks: Vec<Chunk> = Vec::new();
        for _ in 0..*table_section.entry_count() {
            let entry = io.read_u4le().map_err(|e|
                E01Error::ReadError { source: FuckOffKError(e) })?;
            data_offset = (entry & 0x7fffffff) as u64;
            data_offset += *table_section.table_base_offset();
            chunks.push(Chunk {
                data_offset,
                compressed: (entry & 0x80000000) > 0,
                end_offset: None,
            });
        }

        if !ignore_checksums {
            // table footer
            let crc_stored = io.read_u4le().map_err(|e|
                E01Error::ReadError { source: FuckOffKError(e) })?;

            let crc = checksum_reader(
                &io_offsets,
                *table_section.entry_count() as usize * 4
            )?;

            if crc != crc_stored {
                return Err(E01Error::BadChecksum(
                    "Table offset array".into(),
                    crc,
                    crc_stored
                ));
            }
        }

        Ok(chunks)
    }

    fn get_hash_section(
        io: &BytesReader,
        ignore_checksums: bool,
    ) -> Result<OptRc<EwfHashSection>, E01Error> {
        let hash_section =
            EwfHashSection::read_into::<_, EwfHashSection>(io, None, None)
                .map_err(|e| E01Error::DeserializationFailed { name: "EwfHashSection".into(), source: FuckOffKError(e) })?;

        if !ignore_checksums {
            checksum_ok(
                "Hash section",
                io,
                &hash_section._io(),
                *hash_section.checksum(),
            )?;
        }

        Ok(hash_section.clone())
    }

    fn get_digest_section(
        io: &BytesReader,
        ignore_checksums: bool,
    ) -> Result<OptRc<EwfDigestSection>, E01Error> {
        let digest_section = EwfHashSection::read_into::<_, EwfDigestSection>(io, None, None)
            .map_err(|e| E01Error::DeserializationFailed { name: "EwfDigestSection".into(), source: FuckOffKError(e) })?;

        if !ignore_checksums {
            checksum_ok(
                "Digest section",
                io,
                &digest_section._io(),
                *digest_section.checksum(),
            )?;
        }

        Ok(digest_section.clone())
    }

    pub fn read<T: AsRef<Path>>(
        f: T,
        volume: &mut Option<VolumeSection>,
        stored_md5: &mut Option<Vec<u8>>,
        stored_sha1: &mut Option<Vec<u8>>,
        ignore_checksums: bool,
    ) -> Result<Self, E01Error> {
        let io = BytesReader::open(f.as_ref())
            .map_err(|e| E01Error::OpenError { source: FuckOffKError(e) })?;
        let header = SegmentFileHeader::new(&io)?;
        let mut chunks: Vec<Chunk> = Vec::new();
        let mut end_of_sectors = 0;
        let mut current_offset = io.pos();
        while current_offset < io.size() {
            io.seek(current_offset).map_err(|e|
                E01Error::SegmentSeekError {
                    file: f.as_ref().into(),
                    offset: current_offset,
                    source: FuckOffKError(e)
                }
            )?;

            let section =
                EwfSectionDescriptorV1::read_into::<_, EwfSectionDescriptorV1>(&io, None, None)
                    .map_err(|e| E01Error::DeserializationFailed { name: format!("Segment file {} EwfFileHeaderV1", f.as_ref().to_string_lossy()), source: FuckOffKError(e) })?;

            let section_offset = *section.next_offset() as usize;
            let section_size = if *section.size() > 0x4c
            /* header size */
            {
                *section.size() - 0x4c
            } else {
                0
            };
            let section_type_full = section.type_string();
            let section_type = section_type_full.trim_matches(char::from(0));

            if section_type == "disk" || section_type == "volume" {
                *volume = Some(VolumeSection::new(&io, section_size, ignore_checksums)?);
            }

            if section_type == "table" {
                chunks.extend(Segment::read_table(&io, section_size, ignore_checksums)?);
                let chunks_len = chunks.len();
                chunks[chunks_len - 1].end_offset = Some(end_of_sectors);
            }

            if section_type == "sectors" {
                end_of_sectors = io.pos() as u64 + section_size;
            }

            if section_type == "hash" {
                let hash_section = Self::get_hash_section(&io, ignore_checksums)?;
                *stored_md5 = Some(hash_section.md5().clone());
            }

            if section_type == "digest" {
                let digest_section = Self::get_digest_section(&io, ignore_checksums)?;
                *stored_md5 = Some(digest_section.md5().clone());
                *stored_sha1 = Some(digest_section.sha1().clone());
            }

            if current_offset == section_offset || section_type == "done" {
                break;
            }

            current_offset = section_offset;
        }

        let segment = Segment {
            io,
            _header: header,
            chunks,
            end_of_sectors,
        };

        Ok(segment)
    }

    fn read_chunk(
        &self,
        chunk_number: usize,
        chunk_index: usize,
        ignore_checksums: bool,
        buf: &mut [u8]
    ) -> Result<Vec<u8>, E01Error>
    {
        debug_assert!(chunk_index < self.chunks.len());
        let chunk = &self.chunks[chunk_index];
        self.io
            .seek(chunk.data_offset as usize)
            .map_err(|e| E01Error::SeekError { source: FuckOffKError(e) })?;

        let end_offset = if chunk_index == self.chunks.len() - 1 {
            self.end_of_sectors
        }
        else if let Some(end_of_section) = chunk.end_offset {
            end_of_section
        }
        else {
            self.chunks[chunk_index + 1].data_offset
        };

        let mut raw_data = self
            .io
            .read_bytes(end_offset as usize - chunk.data_offset as usize)
            .map_err(|e| E01Error::ReadError { source: FuckOffKError(e) })?;

        if !chunk.compressed {
            if !ignore_checksums {
                // read stored checksum
                let crc_stored = u32::from_le_bytes(
                    raw_data[raw_data.len() - 4..]
                        .try_into()
                        .expect("slice of last 4 bytes not 4 bytes long, wtf")
                );

                // remove stored checksum from data
                raw_data.truncate(raw_data.len() - 4);

                // checksum the data
                let crc = checksum(&raw_data)?;

                if crc != crc_stored {
                    return Err(E01Error::BadChecksum(
                        format!("Chunk {}", chunk_number),
                        crc,
                        crc_stored
                    ));
                }
            }

            Ok(raw_data)
        }
        else {
            let mut decoder = ZlibDecoder::new(&raw_data[..]);
            let mut data = Vec::new();
            decoder
                .read_to_end(&mut data)
                .map_err(|e| E01Error::DecompressionFailed(e))?;
            Ok(data)
        }
    }
}

// Errors should be: ioerror, bad paths, bad input

impl E01Reader {
    pub fn open_glob<T: AsRef<Path>>(
        example_segment_path: T,
        ignore_checksums: bool
    ) -> Result<Self, E01Error>
    {
        Self::open(
            find_segment_paths(&example_segment_path, FileGlobber)
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

        // read first segment, volume section must be contained in it
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

        chunks += seg.chunks.len();
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

            chunks += seg.chunks.len();
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
            ignore_checksums,
            stored_md5,
            stored_sha1,
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
                if chunk_number >= chunks && chunk_number < chunks + s.chunks.len() {
                    *chunk_index = chunk_number - chunks;
                    return true;
                }
                chunks += s.chunks.len();
                false
            })
            .ok_or_else(|| E01Error::BadChunkNumber(chunk_number))
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

        while remaining_buf.len() > 0 && offset < total_size {
            let chunk_number = offset / self.chunk_size();
            debug_assert!(chunk_number < self.volume.chunk_count as usize);
            let mut chunk_index = 0;

            let mut data = self
                .get_segment(chunk_number, &mut chunk_index)?
                .read_chunk(
                    chunk_number,
                    chunk_index,
                    self.ignore_checksums,
                    &mut remaining_buf
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
