use async_trait::async_trait;

use crate::bytessource::BytesSource;
use crate::cache::Cache;

pub struct DummyCache {
    sources: Vec<Box<dyn BytesSource + Send>>
}

impl DummyCache {
    pub fn new() -> Self {
        Self { sources: vec![] }
    }

    pub fn add_source(&mut self, src: Box<dyn BytesSource + Send>) {
        self.sources.push(src);
    }
}

#[async_trait]
impl Cache for DummyCache {
    async fn read(
        &mut self,
        idx: usize,
        off: u64,
        buf: &mut [u8]
    ) -> Result<(), std::io::Error>
    {
        let b = self.sources[idx].read(off, off + buf.len() as u64).await?;
        buf.copy_from_slice(&b);
        Ok(())
    }

    fn end(&self, idx: usize) -> Result<u64, std::io::Error> {
        self.sources.get(idx)
            .ok_or(std::io::Error::other(format!("{idx} out of bounds")))
            .map(|src| src.end())
    }
}
