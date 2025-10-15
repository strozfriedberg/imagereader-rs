use kaitai::KError;

#[derive(Debug, thiserror::Error)]
pub enum IoError {
    #[error("{0}")]
    IoError(#[from] std::io::Error),
    #[error("{0:?}")]
    ReadError(KError),
    #[error("Seek to {0} failed: {1:?}")]
    SeekError(usize, KError)
}

#[derive(Debug, thiserror::Error)]
pub enum LibError {
    #[error("{0}")]
    IoError(#[from] IoError),
    #[error("{0} checksum failed, calculated {1}, expected {2}")]
    BadChecksum(String, u32, u32),
    #[error("Error while deserializing {0} struct: {1:?}")]
    DeserializationFailed(&'static str, KError),
    #[error("Unexpected volume size: {0}")]
    UnexpectedVolumeSize(u64),
    #[error("Unknown compression method value: {0}")]
    UnknownCompressionMethod(u16),
    #[error("Invalid segment file header")]
    InvalidSegmentFileHeader,
}
