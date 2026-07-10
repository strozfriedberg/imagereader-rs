#[derive(Debug, thiserror::Error)]
pub enum InitError {
    #[error("Failed to start tokio Runtime: {0}")]
    TokioRuntimeFailed(std::io::Error),
    #[error("{0}")]
    CacheSetupFailed(std::io::Error),
}

#[derive(Debug, thiserror::Error)]
pub enum OpenErrorKind {
    #[error("{0}")]
    IoError(#[from] std::io::Error),
    #[error("{0}")]
    InitializationFailed(#[from] InitError),
    #[error("Malformed path or URL: {0}")]
    BadPath(String),
    #[error("Unsupported URL scheme: {0}")]
    UnsupportedScheme(String),
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

impl From<std::io::Error> for OpenError {
    fn from(e: std::io::Error) -> Self {
        Self {
            path: "".into(), // set using with_path()
            kind: OpenErrorKind::IoError(e),
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
