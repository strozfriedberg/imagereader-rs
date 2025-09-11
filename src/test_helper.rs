use rand::Rng;
use std::collections::HashMap;

use crate::hasher::{HashType, MultiHasher};

pub fn do_hash<RF>(
    reader: RF,
    image_size: usize,
    random_buf_size: bool
) -> HashMap<HashType, String>
where
    RF: Fn(usize, &mut [u8]) -> usize
{
    let mut hasher = MultiHasher::from([
        HashType::MD5,
        HashType::SHA1,
        HashType::SHA256
    ]);

    let mut buf: Vec<u8> = vec![0; 1048576];
    let mut offset = 0;

    while offset < image_size {
        let buf_size = if random_buf_size {
            rand::rng().random_range(0..buf.len())
        }
        else {
            buf.len()
        };

        let read = reader(offset, &mut buf[..buf_size]);

        if read == 0 {
            break;
        }

        hasher.update(&buf[..read]);

        offset += read;
    }

    hasher.finalize()
        .into_iter()
        .map(|(k, v)| (k, hex::encode(v)))
        .collect()
}
