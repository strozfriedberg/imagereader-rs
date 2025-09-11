use digest::{Digest, DynDigest};
use md5::Md5;
use sha1::Sha1;
use sha2::Sha256;

use std::{
    collections::HashMap,
    fmt,
    str::FromStr
};

#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
pub enum HashType {
    MD5,
    SHA1,
    SHA256
}

impl HashType {
    pub fn hasher(self) -> (Self, Box<dyn DynDigest>) {
        (
            self,
            match self {
                HashType::MD5 => Box::new(Md5::new()),
                HashType::SHA1 => Box::new(Sha1::new()),
                HashType::SHA256 => Box::new(Sha256::new())
            }
        )
    }
}

impl fmt::Display for HashType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            HashType::MD5 => write!(f, "MD5"),
            HashType::SHA1 => write!(f, "SHA1"),
            HashType::SHA256 => write!(f, "SHA256")
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
#[error("Unknown hash type")]
pub struct HashTypeError;

impl FromStr for HashType {
    type Err = HashTypeError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_uppercase().as_ref() {
            "MD5" => Ok(HashType::MD5),
            "SHA1" => Ok(HashType::SHA1),
            "SHA256" => Ok(HashType::SHA256),
            _ => Err(HashTypeError)
        }
    }
}

pub struct MultiHasher {
    hashers: HashMap<HashType, Box<dyn DynDigest>>
}

impl MultiHasher {
    pub fn new<T: IntoIterator<Item = (HashType, Box<dyn DynDigest>)>>(hashers: T) -> Self
    where
        HashMap<HashType, Box<dyn DynDigest>>: From<T>
    {
        Self { hashers: hashers.into() }
    }

    pub fn update(
        &mut self,
        buf: &[u8]
    )
    {
        self.hashers.values_mut().for_each(|h| h.update(buf));
    }

    pub fn finalize(
        self
    ) -> HashMap<HashType, Box<[u8]>>
    {
        self.hashers
            .into_iter()
            .map(|(k, v)| (k, v.finalize()))
            .collect()
    }
}

impl<T: IntoIterator<Item = HashType>> From<T> for MultiHasher {
    fn from(htypes: T) -> Self {
        Self {
            hashers: HashMap::from_iter(
                htypes.into_iter().map(HashType::hasher)
            )
        }
    }
}

#[cfg(test)]
mod test {
    use super::*;

    use hex::{self, FromHex};

    fn md5(s: &str) -> (HashType, Box<[u8]>) {
       (HashType::MD5, <[u8; 16]>::from_hex(s).unwrap().into())
    }

    fn sha1(s: &str) -> (HashType, Box<[u8]>) {
       (HashType::SHA1, <[u8; 20]>::from_hex(s).unwrap().into())
    }

    fn sha256(s: &str) -> (HashType, Box<[u8]>) {
        (HashType::SHA256, <[u8; 32]>::from_hex(s).unwrap().into())
    }

    #[test]
    fn test_hash_nothing() {
        let hasher = MultiHasher::from([
            HashType::MD5,
            HashType::SHA1,
            HashType::SHA256
        ]);

        let exp = HashMap::from([
            md5("d41d8cd98f00b204e9800998ecf8427e"),
            sha1("da39a3ee5e6b4b0d3255bfef95601890afd80709"),
            sha256("e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855")
        ]);

        assert_eq!(hasher.finalize(), exp);
    }

    #[test]
    fn test_hash_something() {
        let mut hasher = MultiHasher::from([
            HashType::MD5,
            HashType::SHA1,
            HashType::SHA256
        ]);

        hasher.update("something".as_bytes());

        let exp = HashMap::from([
            md5("437b930db84b8079c2dd804a71936b5f"),
            sha1("1af17e73721dbe0c40011b82ed4bb1a7dbe3ce29"),
            sha256("3fc9b689459d738f8c88a3a48aa9e33542016b7a4052e001aaa536fca74813cb")
        ]);

        assert_eq!(hasher.finalize(), exp);
    }

    #[test]
    fn test_hash_some_thing() {
        let mut hasher = MultiHasher::from([
            HashType::MD5,
            HashType::SHA1,
            HashType::SHA256,
        ]);

        hasher.update("some".as_bytes());
        hasher.update("thing".as_bytes());

        let exp = HashMap::from([
            md5("437b930db84b8079c2dd804a71936b5f"),
            sha1("1af17e73721dbe0c40011b82ed4bb1a7dbe3ce29"),
            sha256("3fc9b689459d738f8c88a3a48aa9e33542016b7a4052e001aaa536fca74813cb")
        ]);

        assert_eq!(hasher.finalize(), exp);
    }


}
