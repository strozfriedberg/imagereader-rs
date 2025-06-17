use crate::e01_reader::{E01Error, FuckOffKError};

pub fn checksum(bytes: &[u8]) -> Result<u32, E01Error> {
    Ok(adler32::adler32(std::io::Cursor::new(bytes))?)
}
