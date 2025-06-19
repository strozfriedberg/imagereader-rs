use kaitai::KError;

use std::fmt;

#[derive(Debug)]
pub struct FuckOffKError(pub KError);

impl fmt::Display for FuckOffKError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:?}", self.0)
    }
}

impl std::error::Error for FuckOffKError {}

#[derive(Debug, thiserror::Error)]
pub enum IoError {
    #[error("{0}")]
    IoError(#[from] std::io::Error),
    #[error("{0}")]
    ReadError(#[from] FuckOffKError),
    #[error("Seek to {offset} failed: {source}")]
    SeekError {
        offset: usize,
        #[source]
        source: FuckOffKError
    }
}

#[derive(Debug, thiserror::Error)]
pub enum LibError {
    #[error("{0}")]
    IoError(#[from] IoError),
    #[error("{0} checksum failed, calculated {1}, expected {2}")]
    BadChecksum(String, u32, u32),
    #[error("Error while deserializing {name} struct: {source}")]
    DeserializationFailed {
        name: String,
        #[source]
        source: FuckOffKError
    },
    #[error("Unexpected volume size: {0}")]
    UnexpectedVolumeSize(u64),
    #[error("Unknown compression method value: {0}")]
    UnknownCompressionMethod(u16),
    #[error("Invalid segment file")]
    InvalidSegmentFile,
    #[error("Decompression failed")]
    DecompressionFailed(#[source] std::io::Error)
}
