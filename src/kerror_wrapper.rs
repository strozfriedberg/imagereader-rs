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
