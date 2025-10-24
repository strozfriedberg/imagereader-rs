use std::sync::{Arc, Mutex};
use tokio::runtime::Runtime;

use crate::cache::Cache;
use crate::workersource::WorkerSource;

pub struct CacheWorkerSource {
    pub cache: Arc<Mutex<dyn Cache + Send>>,
    pub runtime: Arc<Runtime>,
    pub idx: usize
}

impl WorkerSource for CacheWorkerSource {
    fn read(
        &mut self,
        off: u64,
        buf: &mut [u8]
    ) -> Result<(), std::io::Error>
    {
        let mut cache = self.cache.lock().unwrap();
        self.runtime.block_on(cache.read(self.idx, off, buf))
    }
}
