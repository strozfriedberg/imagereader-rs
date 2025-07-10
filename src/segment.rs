use crate::generated::ewf_file_header_v1::EwfFileHeaderV1;
use crate::generated::ewf_file_header_v2::EwfFileHeaderV2;
use crate::error::{IoError, LibError};

use kaitai::{BytesReader, KStream, KStruct};
use std::convert::TryFrom;

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
