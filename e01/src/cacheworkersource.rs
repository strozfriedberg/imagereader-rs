use std::sync::{Arc, Mutex};
use tokio::runtime::Runtime;

use imagesource::{Cache, ReadTrace};
use crate::workersource::WorkerSource;

pub struct CacheWorkerSource {
    pub cache: Arc<dyn Cache>,
    pub runtime: Arc<Runtime>,
    pub idx: usize,
    pub foyer_trace: Option<Arc<Mutex<ReadTrace>>>,
}

impl WorkerSource for CacheWorkerSource {
    fn read(&mut self, off: u64, buf: &mut [u8]) -> Result<(), std::io::Error> {
        let mut local = ReadTrace::default();
        self.runtime
            .block_on(self.cache.read(self.idx, off, buf, &mut local))?;
        if local.foyer_miss
            && let Some(shared) = &self.foyer_trace
        {
            shared.lock().unwrap().foyer_miss = true;
        }
        Ok(())
    }
}
