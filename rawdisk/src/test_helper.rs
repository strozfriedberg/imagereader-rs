#![cfg(test)]

use sha1::{Digest, Sha1};

pub fn do_hash<RF>(mut reader: RF, image_size: u64) -> String
where
    RF: FnMut(u64, &mut [u8]) -> usize,
{
    let mut hasher = Sha1::new();
    let mut buf: Vec<u8> = vec![0; 1048576];
    let mut offset = 0;

    while offset < image_size {
        let read = reader(offset, &mut buf);

        if read == 0 {
            break;
        }

        hasher.update(&buf[..read]);

        offset += read as u64;
    }

    let result = hasher.finalize();
    format!("{:x}", result)
}
