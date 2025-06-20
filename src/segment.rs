use crate::generated::ewf_file_header_v1::EwfFileHeaderV1;
use crate::generated::ewf_file_header_v2::EwfFileHeaderV2;
use crate::error::{IoError, LibError};
use crate::sec_read::Chunk;

use flate2::read::ZlibDecoder;
use kaitai::{BytesReader, KStream, KStruct};
use std::{
    convert::TryFrom,
    io::Read,
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
pub struct SegmentFileHeader {
    major_version: u8,
    minor_version: u8,
    compr_method: CompressionMethod,
    segment_number: u16,
}

fn try_ewf_file_header_v1(
    io: &BytesReader
) -> Result<SegmentFileHeader, LibError>
{
    match EwfFileHeaderV1::read_into::<_, EwfFileHeaderV1>(io, None, None) {
        Ok(h) => Ok(SegmentFileHeader {
            major_version: 1,
            minor_version: 0,
            compr_method: CompressionMethod::Deflate,
            segment_number: *h.segment_number(),
        }),
        Err(e) => Err(LibError::DeserializationFailed("EwfFileHeaderV1", e))
    }
}

fn try_ewf_file_header_v2(
    io: &BytesReader
) -> Result<SegmentFileHeader, LibError>
{
    match EwfFileHeaderV2::read_into::<_, EwfFileHeaderV2>(io, None, None) {
        Ok(h) => Ok(SegmentFileHeader {
            major_version: *h.major_version(),
            minor_version: *h.minor_version(),
            compr_method: (*h.compression_method()).try_into()?,
            segment_number: *h.segment_number(),
        }),
        Err(e) => Err(LibError::DeserializationFailed("EwfFileHeaderV2", e))
    }
}

impl SegmentFileHeader {
    pub fn new(io: &BytesReader) -> Result<Self, LibError> {
        let first_bytes = io
            .read_bytes(8)
            .map_err(IoError::ReadError)?;

        io.seek(0).map_err(|e| IoError::SeekError(0, e))?;

        // read file header
        match first_bytes.as_slice() {
            [0x45, 0x56, 0x46, 0x09, 0x0d, 0x0a, 0xff, 0x00] | // EWF, EWF-E01, EWF-S01
            [0x4c, 0x56, 0x46, 0x09, 0x0d, 0x0a, 0xff, 0x00] // EWF-L01
                => try_ewf_file_header_v1(io),
            [0x45, 0x56, 0x46, 0x32, 0x0d, 0x0a, 0x81, 0x00] | // EVF2
            [0x4c, 0x45, 0x46, 0x32, 0x0d, 0x0a, 0x81, 0x00] // LEF2
                => try_ewf_file_header_v2(io),
            _ => Err(LibError::InvalidSegmentFileHeader)
        }
    }
}

#[derive(Debug)]
pub struct Segment {
    pub io: BytesReader,
    pub _header: SegmentFileHeader,
    pub chunks: Vec<Chunk>,
    pub end_of_sectors: u64
}

impl Segment {
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
            .map_err(|e| IoError::SeekError(chunk.data_offset as usize, e))?;

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
            .map_err(IoError::ReadError)?;

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
                    .map_err(IoError::IoError)?;

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
            let mut data = vec![];
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
