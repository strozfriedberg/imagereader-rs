use std::{
    io::{Read, Seek, SeekFrom},
    sync::Arc,
};
use tokio::runtime::Runtime;

use crate::cache::Cache;
use crate::io_log::{IoLog, ReadTimer, ReadTrace, chunk_cache_label};

#[derive(Clone)]
pub struct CacheReadSeek {
    cache: Arc<dyn Cache>,
    runtime: Arc<Runtime>,
    idx: usize,
    pos: u64,
    io_log: Option<Arc<IoLog>>,
}

impl CacheReadSeek {
    pub fn new(
        cache: Arc<dyn Cache>,
        runtime: Arc<Runtime>,
        idx: usize,
        _len: u64,
        io_log: Option<Arc<IoLog>>,
    ) -> Self {
        Self {
            cache,
            runtime,
            idx,
            pos: 0,
            io_log,
        }
    }
}

impl Read for CacheReadSeek {
    fn read(&mut self, buf: &mut [u8]) -> Result<usize, std::io::Error> {
        // check that we don't read past the end of the source
        let send = self.cache.end(self.idx)?;
        let rend = send.min(self.pos + buf.len() as u64);

        let len = (rend - self.pos) as usize;

        if len > 0 {
            let timer = self.io_log.as_ref().map(|_| ReadTimer::start());
            let read_offset = self.pos;
            let mut trace = ReadTrace::default();
            self.runtime.block_on(self.cache.read(
                self.idx,
                self.pos,
                &mut buf[..len],
                &mut trace,
            ))?;
            self.pos = rend;

            // vmdk has no secondary decoded-chunk cache like e01's, so there's
            // no chunk-level hit/miss to report -- only the foyer tier.
            if let Some(log) = &self.io_log {
                let dur_us = timer.as_ref().map(ReadTimer::elapsed_us).unwrap_or(0);
                log.log_read(
                    read_offset,
                    len,
                    dur_us,
                    trace.foyer_label(),
                    chunk_cache_label(None),
                );
            }
        }

        Ok(len)
    }
}

impl Seek for CacheReadSeek {
    fn seek(&mut self, pos: SeekFrom) -> Result<u64, std::io::Error> {
        let end = self.cache.end(self.idx)?;

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
