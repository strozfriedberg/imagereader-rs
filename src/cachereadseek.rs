use std::{
    io::{Read, Seek, SeekFrom},
    sync::{Arc, Mutex},
};
use tokio::runtime::Runtime;

use crate::cache::Cache;
use crate::io_log::ReadTrace;

pub struct CacheReadSeek {
    cache: Arc<Mutex<dyn Cache + Send>>,
    runtime: Arc<Runtime>,
    idx: usize,
    pos: u64,
    len: u64,
}

impl CacheReadSeek {
    pub fn new(
        cache: Arc<Mutex<dyn Cache + Send>>,
        runtime: Arc<Runtime>,
        idx: usize,
        len: u64,
    ) -> Self {
        Self {
            cache,
            runtime,
            idx,
            pos: 0,
            len,
        }
    }
}

impl Read for CacheReadSeek {
    fn read(&mut self, buf: &mut [u8]) -> Result<usize, std::io::Error> {
        let remaining = self.len.saturating_sub(self.pos);
        if remaining == 0 || buf.is_empty() {
            return Ok(0);
        }
        let n = (buf.len() as u64).min(remaining) as usize;
        let buf = &mut buf[..n];

        let mut cache = self.cache.lock().expect("poisoned");
        let mut trace = ReadTrace::default();
        self.runtime
            .block_on(cache.read(self.idx, self.pos, buf, &mut trace))?;

        self.pos += n as u64;
        Ok(n)
    }
}

impl Seek for CacheReadSeek {
    fn seek(&mut self, pos: SeekFrom) -> Result<u64, std::io::Error> {
        let end = self.cache.lock().expect("poisoned").end(self.idx)?;

        let (base, offset) = match pos {
            SeekFrom::Start(n) => (n, 0),
            SeekFrom::End(n) => (end, n),
            SeekFrom::Current(n) => (self.pos, n),
        };

        self.pos = match base.checked_add_signed(offset) {
            Some(n) if n <= end => Ok(n),
            Some(_) => Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "invalid seek past end",
            )),
            None => Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "invalid seek to a negative or overflowing position",
            )),
        }?;

        Ok(self.pos)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bytessource::BytesSource;
    use async_trait::async_trait;

    struct FixedCache {
        data: Vec<u8>,
    }

    #[async_trait]
    impl Cache for FixedCache {
        async fn read(
            &mut self,
            _idx: usize,
            off: u64,
            buf: &mut [u8],
            _trace: &mut ReadTrace,
        ) -> Result<(), std::io::Error> {
            let off = off as usize;
            buf.copy_from_slice(&self.data[off..off + buf.len()]);
            Ok(())
        }

        fn end(&self, _idx: usize) -> Result<u64, std::io::Error> {
            Ok(self.data.len() as u64)
        }

        fn add_source(&mut self, _idx: usize, _src: Box<dyn BytesSource + Send + Sync>) {}
    }

    #[test]
    fn read_clamps_to_end_and_reports_real_count() {
        let data: Vec<u8> = (0..100).map(|i| i as u8).collect();
        let cache: Arc<Mutex<dyn Cache + Send>> =
            Arc::new(Mutex::new(FixedCache { data: data.clone() }));
        let runtime = Arc::new(tokio::runtime::Runtime::new().unwrap());
        let mut crs = CacheReadSeek::new(cache, runtime, 0, data.len() as u64);

        crs.seek(SeekFrom::Start(90)).unwrap();
        let mut buf = [0xAAu8; 64];
        let n = crs.read(&mut buf).unwrap();
        assert_eq!(n, 10, "read past end must return only the remaining bytes");
        assert_eq!(&buf[..10], &data[90..]);

        let n = crs.read(&mut buf).unwrap();
        assert_eq!(n, 0, "read at end must return 0, not fabricate bytes");
    }
}
