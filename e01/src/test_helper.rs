use std::collections::HashMap;
use tracing::trace;

use crate::hasher::{HashType, MultiHasher};

pub fn do_hash<RF>(mut reader: RF, image_size: u64) -> HashMap<HashType, String>
where
    RF: FnMut(u64, &mut [u8]) -> usize,
{
    let htypes = [HashType::MD5, HashType::SHA1, HashType::SHA256];

    let hasher = MultiHasher::new(htypes, vec![0; 1024 * 1024]);

    let mut buf: Vec<u8> = vec![0; 1024 * 1024];
    let mut offset = 0;

    while offset < image_size {
        let read = reader(offset, &mut buf);

        if read == 0 {
            break;
        }

        buf = hasher.update(buf, read);

        offset += read as u64;
        trace!("hashed to {offset}");
    }

    hasher
        .finalize()
        .into_iter()
        .map(|(k, v)| (k, hex::encode(v)))
        .collect()
}
