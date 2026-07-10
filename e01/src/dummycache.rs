use std::sync::{Arc, RwLock};

use async_trait::async_trait;

use crate::{
    bytessource::BytesSource, cache::Cache, io_log::ReadTrace, placeholdersource::PlaceholderSource,
};

#[allow(dead_code)]
pub struct DummyCache {
    sources: RwLock<Vec<Arc<dyn BytesSource + Send + Sync>>>,
}

impl DummyCache {
    #[allow(dead_code)]
    pub fn new() -> Self {
        Self {
            sources: RwLock::new(vec![]),
        }
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
        let source = self
            .sources
            .read()
            .expect("sources lock poisoned")
            .get(idx)
            .cloned()
            .ok_or(std::io::Error::other(format!("{idx} out of bounds")))?;
        let b = source.read(off, off + buf.len() as u64).await?;
        buf.copy_from_slice(&b);
        Ok(())
    }

    fn end(&self, idx: usize) -> Result<u64, std::io::Error> {
        self.sources
            .read()
            .expect("sources lock poisoned")
            .get(idx)
            .ok_or(std::io::Error::other(format!("{idx} out of bounds")))
            .map(|src| src.end())
    }

    fn add_source(&self, idx: usize, src: Box<dyn BytesSource + Send + Sync>) {
        let mut sources = self.sources.write().expect("sources lock poisoned");
        if sources.len() <= idx {
            sources.resize_with(idx + 1, || Arc::new(PlaceholderSource));
        }
        sources[idx] = Arc::from(src);
    }
}
