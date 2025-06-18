use crate::generated::ewf_file_header_v1::EwfFileHeaderV1;
use crate::generated::ewf_file_header_v2::EwfFileHeaderV2;
use crate::error::{FuckOffKError, IoError, LibError};
use crate::sec_read::{Chunk, Section, SectionIterator, VolumeSection};

use flate2::read::ZlibDecoder;
use kaitai::{BytesReader, KStream, KStruct};
use std::{
    convert::TryFrom,
    io::Read,
    path::Path
};

#[derive(Debug)]
enum CompressionMethod {
    None = 0,
    Deflate = 1,
    Bzip = 2,
}

impl TryFrom<u16> for CompressionMethod {
    type Error = LibError;

    fn try_from(v: u16) -> Result<Self, Self::Error> {
        match v {
            x if x == Self::None as u16 => Ok(Self::None),
            x if x == Self::Deflate as u16 => Ok(Self::Deflate),
            x if x == Self::Bzip as u16 => Ok(Self::Bzip),
            _ => Err(LibError::UnknownCompressionMethod(v))
        }
    }
}

#[derive(Debug)]
struct SegmentFileHeader {
    major_version: u8,
    minor_version: u8,
    compr_method: CompressionMethod,
    segment_number: u16,
}

impl SegmentFileHeader {
    pub fn new(io: &BytesReader) -> Result<Self, LibError> {
        let first_bytes = io
            .read_bytes(8)
            .map_err(|e| LibError::IoError(IoError::ReadError(FuckOffKError(e))))?;

        io.seek(0)
            .map_err(|e| LibError::IoError(IoError::SeekError { offset: 0, source: FuckOffKError(e) }))?;

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
                    Err(LibError::DeserializationFailed { name: "EwfFileHeaderV1".into(), source: FuckOffKError(e) })
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
                    Err(LibError::DeserializationFailed { name: "EwfFileHeaderV2".into(), source: FuckOffKError(e) })
                }
            }
        }
        else {
            Err(LibError::InvalidSegmentFile)
        }
    }
}

#[derive(Debug)]
pub struct Segment {
    io: BytesReader,
    _header: SegmentFileHeader,
    chunks: Vec<Chunk>,
    end_of_sectors: u64,
}

impl Segment {
    pub fn read<T: AsRef<Path>>(
        f: T,
        volume: &mut Option<VolumeSection>,
        stored_md5: &mut Option<Vec<u8>>,
        stored_sha1: &mut Option<Vec<u8>>,
        ignore_checksums: bool,
    ) -> Result<Self, LibError> {
        let io = BytesReader::open(f.as_ref())
            .map_err(|e| LibError::OpenError(FuckOffKError(e)))?;
        let header = SegmentFileHeader::new(&io)?;
        let mut chunks: Vec<Chunk> = Vec::new();
        let mut end_of_sectors = 0;

        for section in SectionIterator::new(&io, ignore_checksums) {
            match section? {
                Section::Volume(v) => *volume = Some(v),
                Section::Table(t) => {
                    chunks.extend(t);
                    let chunks_len = chunks.len();
                    chunks[chunks_len - 1].end_offset = Some(end_of_sectors);
                },
                Section::Sectors(eos) => end_of_sectors = eos,
                Section::Hash(h) => *stored_md5 = Some(h.md5().clone()),
                Section::Digest(d) => {
                    *stored_md5 = Some(d.md5().clone());
                    *stored_sha1 = Some(d.sha1().clone());
                },
                Section::Done => break,
                _ => {}
            }
        }

        Ok(Segment {
            io,
            _header: header,
            chunks,
            end_of_sectors
        })
    }

    pub fn read_chunk(
        &self,
        chunk_number: usize,
        chunk_index: usize,
        ignore_checksums: bool,
        buf: &mut [u8]
    ) -> Result<Vec<u8>, LibError>
    {
        debug_assert!(chunk_index < self.chunks.len());
        let chunk = &self.chunks[chunk_index];
        self.io
            .seek(chunk.data_offset as usize)
            .map_err(|e| LibError::IoError(IoError::SeekError { offset: chunk.data_offset as usize, source: FuckOffKError(e) }))?;

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
            .map_err(|e| LibError::IoError(IoError::ReadError(FuckOffKError(e))))?;

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
                let crc = adler32::adler32(std::io::Cursor::new(&raw_data))
                    .map_err(|e| LibError::IoError(IoError::IoError(e)))?;

                if crc != crc_stored {
                    return Err(LibError::BadChecksum(
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
                .map_err(LibError::DecompressionFailed)?;
            Ok(data)
        }
    }

    pub fn chunk_count(&self) -> usize {
        self.chunks.len()
    }
}
