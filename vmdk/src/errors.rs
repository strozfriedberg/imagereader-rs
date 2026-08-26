pub use imagesource::errors::InitError;

use crate::{extent_description::ParseExtentDescriptionError, header::FileType};

#[derive(Debug, thiserror::Error)]
pub enum DescriptorError {
    #[error("{0}")]
    ParseExtentDescription(#[from] ParseExtentDescriptionError),
    #[error("failed to recognize descriptor")]
    UnrecognizedDescriptor,
    #[error("a {0:?} file has no descriptor and cannot be opened directly")]
    NoDescriptor(FileType),
}

#[derive(Debug, thiserror::Error)]
#[error("Error while deserializing {0} struct: {1:?}")]
pub struct DeserializationError(pub &'static str, pub std::io::Error);

#[derive(Debug, thiserror::Error)]
pub enum OpenErrorKind {
    #[error("{0}")]
    IoError(#[from] std::io::Error),
    #[error("Expected size of parent extent descriptor {0}, actual {1}")]
    BadParentExtentDescriptorSize(u64, u64),
    #[error("Error reading descriptor: {0}")]
    DescriptorError(#[from] DescriptorError),
    #[error("{0}")]
    DeserializationFailed(#[from] DeserializationError),
    #[error("No KDMV or COWD headers detected")]
    InvalidFileHeader,
    #[error("{0}")]
    InitializationFailed(#[from] InitError),
    #[error("Malformed path or URL: {0}")]
    BadPath(String),
    #[error("Unsupported extent kind: {0}")]
    UnsupportedExtentKind(String),
    #[error("Parent extent chain contains a cycle at {0}")]
    ParentChainCycle(String),
    #[error("Extent sizes overflow the image size")]
    ImageSizeOverflow,
    #[error("{0}")]
    Source(imagesource::OpenErrorKind),
}

#[derive(Debug, thiserror::Error)]
#[error("{path}: {kind}")]
pub struct OpenError {
    pub path: String,
    #[source]
    pub kind: OpenErrorKind,
}

impl From<OpenErrorKind> for OpenError {
    fn from(e: OpenErrorKind) -> Self {
        Self {
            path: "".into(), // set using with_path()
            kind: e,
        }
    }
}

impl From<DescriptorError> for OpenError {
    fn from(e: DescriptorError) -> Self {
        Self {
            path: "".into(), // set using with_path()
            kind: OpenErrorKind::DescriptorError(e),
        }
    }
}

impl From<DeserializationError> for OpenError {
    fn from(e: DeserializationError) -> Self {
        Self {
            path: "".into(), // set using with_path()
            kind: OpenErrorKind::DeserializationFailed(e),
        }
    }
}

impl From<std::io::Error> for OpenError {
    fn from(e: std::io::Error) -> Self {
        Self {
            path: "".into(), // set using with_path()
            kind: OpenErrorKind::IoError(e),
        }
    }
}

impl From<imagesource::OpenError> for OpenError {
    fn from(e: imagesource::OpenError) -> Self {
        Self {
            path: e.path,
            kind: OpenErrorKind::Source(e.kind),
        }
    }
}

impl From<InitError> for OpenError {
    fn from(e: InitError) -> Self {
        Self {
            path: "".into(), // set using with_path()
            kind: OpenErrorKind::InitializationFailed(e),
        }
    }
}

impl OpenError {
    pub fn with_path<T: AsRef<str>>(self, path: T) -> Self {
        Self {
            path: path.as_ref().into(),
            kind: self.kind,
        }
    }
}
