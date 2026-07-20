use kaitai::KError;

#[derive(Debug, thiserror::Error)]
pub enum IoError {
    #[error("{0}")]
    Io(#[from] std::io::Error),
    #[error("{0:?}")]
    Read(KError),
    #[error("Seek to {0} failed: {1:?}")]
    Seek(usize, KError),
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
    #[error("Corrupt chunk table: {0}")]
    CorruptChunkTable(String),
    #[error("Table section claims {0} entries, maximum is {MAX_TABLE_ENTRIES}")]
    TooManyTableEntries(u32),
}

/// EWF caps table sections at 65534 entries; anything larger is corrupt.
pub const MAX_TABLE_ENTRIES: u32 = 65534;
