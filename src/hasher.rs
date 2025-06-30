use digest::DynDigest;

pub struct MultiHasher {
    hashers: Vec<Box<dyn DynDigest>>
}

impl MultiHasher {
    pub fn update(
        &mut self,
        buf: &[u8]
    )
    {
        self.hashers.iter_mut().for_each(|h| h.update(buf));
    }

    pub fn finalize(
        self
    ) -> Vec<Box<[u8]>>
    {
        self.hashers.into_iter().map(DynDigest::finalize).collect()
    }
}

#[cfg(test)]
mod test {
    use digest::Digest;
    use hex;
    use hex::FromHex;
    use md5::Md5;
    use sha1::Sha1;
    use sha2::Sha256;

    use super::*;

    fn md5(s: &str) -> Box<[u8]> {
        <[u8; 16]>::from_hex(s).unwrap().into()
    }

    fn sha1(s: &str) -> Box<[u8]> {
        <[u8; 20]>::from_hex(s).unwrap().into()
    }

    fn sha256(s: &str) -> Box<[u8]> {
        <[u8; 32]>::from_hex(s).unwrap().into()
    }

    #[test]
    fn test_hash_nothing() { 
        let mut hasher = MultiHasher {
            hashers: vec![
                Box::new(Md5::new()),
                Box::new(Sha1::new()),
                Box::new(Sha256::new())
            ]
        };

        let exp: Vec<Box<[u8]>> = vec![
            md5("d41d8cd98f00b204e9800998ecf8427e"),
            sha1("da39a3ee5e6b4b0d3255bfef95601890afd80709"),
            sha256("e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855")
        ];

        assert_eq!(hasher.finalize(), exp);
    }  

    #[test]
    fn test_hash_something() {
        let mut hasher = MultiHasher {
            hashers: vec![
                Box::new(Md5::new()),
                Box::new(Sha1::new()),
                Box::new(Sha256::new())
            ]
        };

        hasher.update("something".as_bytes());
    
        let exp: Vec<Box<[u8]>> = vec![
            md5("437b930db84b8079c2dd804a71936b5f"),
            sha1("1af17e73721dbe0c40011b82ed4bb1a7dbe3ce29"),
            sha256("3fc9b689459d738f8c88a3a48aa9e33542016b7a4052e001aaa536fca74813cb")
        ];

        assert_eq!(hasher.finalize(), exp);
    }

    #[test]
    fn test_hash_some_thing() {
        let mut hasher = MultiHasher {
            hashers: vec![
                Box::new(Md5::new()),
                Box::new(Sha1::new()),
                Box::new(Sha256::new())
            ]
        };

        hasher.update("some".as_bytes());
        hasher.update("thing".as_bytes());
    
        let exp: Vec<Box<[u8]>> = vec![
            md5("437b930db84b8079c2dd804a71936b5f"),
            sha1("1af17e73721dbe0c40011b82ed4bb1a7dbe3ce29"),
            sha256("3fc9b689459d738f8c88a3a48aa9e33542016b7a4052e001aaa536fca74813cb")
        ];

        assert_eq!(hasher.finalize(), exp);
    }
}
