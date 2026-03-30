use digest::{Digest, DynDigest};
use md5::Md5;
use sha1::Sha1;
use sha2::Sha256;
use std::{
    collections::HashMap,
    fmt,
    str::FromStr,
    sync::{
        Arc,
        mpsc::{self, Receiver, SyncSender}
    },
    thread::JoinHandle
};

#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
pub enum HashType {
    MD5,
    SHA1,
    SHA256
}

impl HashType {
    pub fn hasher(self) -> Box<dyn DynDigest> {
        match self {
            HashType::MD5 => Box::new(Md5::new()),
            HashType::SHA1 => Box::new(Sha1::new()),
            HashType::SHA256 => Box::new(Sha256::new())
        }
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
    handles: Vec<(
        HashType,
        SyncSender<(usize, Arc<Vec<u8>>)>,
        Receiver<Arc<Vec<u8>>>,
        JoinHandle<Box<[u8]>>
    )>
}

impl MultiHasher {

    pub fn new<T>(
        htypes: T,
        buf: Vec<u8>
    ) -> Self
    where
        T: IntoIterator<Item = HashType>
    {
        let buf = Arc::new(buf);

        // create a worker thread and hasher for each hash type
        let handles = htypes
            .into_iter()
            .map(|htype| {
                let (full_tx, full_rx) = mpsc::sync_channel::<(usize, Arc<Vec<u8>>)>(1);
                let (empty_tx, empty_rx) = mpsc::sync_channel::<Arc<Vec<u8>>>(1);

                // prime the empty channel with a buffer
                empty_tx.send(buf.clone())
                    .expect("channel cannot be closed");

                (
                    htype,
                    full_tx,
                    empty_rx,
                    std::thread::spawn(move || {
                        let mut h = htype.hasher();

                        // hash until the full channel is closed
                        while let Ok((r, buf)) = full_rx.recv() {
                            h.update(&buf[..r]);
                            // return the buffer via the empty channel
                            if empty_tx.send(buf).is_err() {
                                break;
                            }
                        }

                        // done, return the hash
                        h.finalize()
                    })
                )
            })
            .collect::<Vec<_>>();

        Self { handles }
    }

    pub fn update(
        &self,
        buf: Vec<u8>,
        len: usize
    ) -> Vec<u8>
    {
        if self.handles.is_empty() {
            buf
        }
        else {
            // send the full buffer to the hashers
            {
                let buf = Arc::new(buf);
                self.handles
                    .iter()
                    .for_each(|(_, full_tx, _, _)| {
                        full_tx.send((len, buf.clone()))
                            .expect("worker cannot have disconnected");
                    });
            }

            // return the empty buffer to the caller
            Arc::into_inner(
                self.handles
                    .iter()
                    .map(|(_, _, empty_rx, _)| empty_rx.recv()
                        .expect("worker cannot fail to return the buffer")
                    )
                    // leave one ref to buffer, drop the rest
                    .reduce(|_, b| b)
                    .expect("handles is not empty")
            )
            .expect("we are the only owner of the buffer")
        }
    }

    pub fn finalize(
        self
    ) -> HashMap<HashType, Box<[u8]>>
    {
        self.handles
            .into_iter()
            // drop the channels to let the workers progress
            .map(|(t, _, _, h)| (t, h))
            // wait for the workers to finish
            .map(|(t, h)| (
                t,
                h.join()
                    // propagate thread panics
                    .unwrap_or_else(|e| std::panic::resume_unwind(e))
            ))
            .collect::<HashMap<_, _>>()
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
        let htypes = [
            HashType::MD5,
            HashType::SHA1,
            HashType::SHA256
        ];

        let hasher = MultiHasher::new(htypes, vec![0; 0]);

        let exp = HashMap::from([
            md5("d41d8cd98f00b204e9800998ecf8427e"),
            sha1("da39a3ee5e6b4b0d3255bfef95601890afd80709"),
            sha256("e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855")
        ]);

        assert_eq!(hasher.finalize(), exp);
    }

    #[test]
    fn test_hash_something() {
        let htypes = [
            HashType::MD5,
            HashType::SHA1,
            HashType::SHA256
        ];

        let hasher = MultiHasher::new(htypes, vec![0; 0]);

        let buf = "something".as_bytes().to_vec();
        let len = buf.len();

        hasher.update(buf, len);

        let exp = HashMap::from([
            md5("437b930db84b8079c2dd804a71936b5f"),
            sha1("1af17e73721dbe0c40011b82ed4bb1a7dbe3ce29"),
            sha256("3fc9b689459d738f8c88a3a48aa9e33542016b7a4052e001aaa536fca74813cb")
        ]);

        assert_eq!(hasher.finalize(), exp);
    }

    #[test]
    fn test_hash_some_thing() {
        let htypes = [
            HashType::MD5,
            HashType::SHA1,
            HashType::SHA256
        ];

        let hasher = MultiHasher::new(htypes, vec![0; 8]);

        let mut buf = Vec::with_capacity(8);
        buf.extend("some".as_bytes());
        let len = buf.len();

        buf = hasher.update(buf, len);

        buf.clear();
        buf.extend("thing".as_bytes());
        let len = buf.len();

        hasher.update(buf, len);

        let exp = HashMap::from([
            md5("437b930db84b8079c2dd804a71936b5f"),
            sha1("1af17e73721dbe0c40011b82ed4bb1a7dbe3ce29"),
            sha256("3fc9b689459d738f8c88a3a48aa9e33542016b7a4052e001aaa536fca74813cb")
        ]);

        assert_eq!(hasher.finalize(), exp);
    }
}
