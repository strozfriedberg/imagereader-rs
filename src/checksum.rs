use crate::e01_reader::{E01Error, FuckOffKError};

use kaitai::{BytesReader, KStream};

pub fn checksum(bytes: &[u8]) -> Result<u32, E01Error> {
    Ok(adler32::adler32(std::io::Cursor::new(bytes))?)
}

pub fn checksum_reader(
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

pub fn checksum_ok(
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
