use async_trait::async_trait;

use crate::{
    bytessource::BytesSource, cache::Cache, foyercache::short_read_error, io_log::ReadTrace,
    source_slot::SourceSlots,
};

#[derive(Default)]
pub struct DummyCache {
    sources: SourceSlots,
}

impl DummyCache {
    pub fn new() -> Self {
        Self::default()
    }
}

#[async_trait]
impl Cache for DummyCache {
    async fn read(
        &self,
        idx: usize,
        off: u64,
        buf: &mut [u8],
        _trace: &mut ReadTrace,
    ) -> Result<(), std::io::Error> {
        let source = self.sources.get(idx)?;
        let b = source.read(off, off + buf.len() as u64).await?;
        // A source that returns fewer bytes than requested (a truncated or
        // corrupt segment) must be an error, not a copy_from_slice panic.
        if b.len() != buf.len() {
            return Err(short_read_error(idx, off, buf.len(), b.len() as u64));
        }
        buf.copy_from_slice(&b);
        Ok(())
    }

    fn end(&self, idx: usize) -> Result<u64, std::io::Error> {
        self.sources.get(idx).map(|src| src.end())
    }

    fn add_source(&self, idx: usize, src: Box<dyn BytesSource + Send + Sync>) {
        self.sources.set(idx, src);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::future::{BoxFuture, FutureExt};

    /// Always returns fewer bytes than asked for, as a truncated segment would.
    struct ShortSource;

    impl BytesSource for ShortSource {
        fn read(&self, beg: u64, end: u64) -> BoxFuture<'static, Result<Vec<u8>, std::io::Error>> {
            let wanted = (end - beg) as usize;
            // hand back one byte fewer than requested
            async move { Ok(vec![0u8; wanted.saturating_sub(1)]) }.boxed()
        }

        fn end(&self) -> u64 {
            1024
        }
    }

    /// A short read from the backing source must be an UnexpectedEof error, not
    /// a copy_from_slice length-mismatch panic.
    #[tokio::test]
    async fn short_read_is_an_error_not_a_panic() {
        let cache = DummyCache::new();
        cache.add_source(0, Box::new(ShortSource));

        let mut buf = [0u8; 32];
        let mut trace = ReadTrace::default();
        let err = cache.read(0, 0, &mut buf, &mut trace).await.unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::UnexpectedEof);
    }
}
