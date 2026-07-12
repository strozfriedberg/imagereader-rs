use async_trait::async_trait;

use crate::{bytessource::BytesSource, cache::Cache, io_log::ReadTrace, source_slot::SourceSlots};

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
