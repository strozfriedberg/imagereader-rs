use async_trait::async_trait;
use foyer::{
    BlockEngineBuilder,
    DeviceBuilder,
    FsDeviceBuilder,
    HybridCacheBuilder
};
use tracing::trace;

use crate::cache::Cache;
use crate::filesource::FileSource;
use crate::foyercache::FoyerCache;

#[derive(Debug)]
pub struct FileFoyerCache {
    cache: FoyerCache<FileSource>
}

impl FileFoyerCache {
    pub fn new() -> Self {

        let dir = "cache";

        let device = FsDeviceBuilder::new(dir)
            .with_capacity(256 * 1024 * 1024)
            .build()
            .unwrap();

        let rt = tokio::runtime::Runtime::new().unwrap();
        let fc = rt.block_on(
            HybridCacheBuilder::new()
                .memory(64 * 1024 * 1024)
                .storage()
                .with_engine_config(BlockEngineBuilder::new(device))
                .build()
        ).unwrap();

        Self {
            cache: FoyerCache::new(fc, 1024 * 1024)
        }
    }

    pub fn add_source(&mut self, fs: FileSource) {
        self.cache.add_source(fs);
    }
}

#[async_trait]
impl Cache for FileFoyerCache {
    async fn read(
        &mut self,
        idx: usize,
        off: u64,
        buf: &mut [u8]
    ) -> Result<(), std::io::Error>
    {
        self.cache.read(idx, off, buf).await
    }

    fn end(&self, idx: usize) -> Result<u64, std::io::Error> {
        self.cache.end(idx)
    }
}
