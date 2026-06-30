use std::sync::{Arc, Mutex};
use tokio::runtime::Runtime;

use crate::cache::Cache;
use crate::io_log::ReadTrace;
use crate::workersource::WorkerSource;

pub struct CacheWorkerSource {
    pub cache: Arc<Mutex<dyn Cache + Send>>,
    pub runtime: Arc<Runtime>,
    pub idx: usize,
    pub foyer_trace: Option<Arc<Mutex<ReadTrace>>>,
}

impl WorkerSource for CacheWorkerSource {
    fn read(&mut self, off: u64, buf: &mut [u8]) -> Result<(), std::io::Error> {
        let mut cache = self.cache.lock().unwrap();
        let mut local = ReadTrace::default();
        self.runtime
            .block_on(cache.read(self.idx, off, buf, &mut local))?;
        if local.foyer_miss {
            if let Some(shared) = &self.foyer_trace {
                shared.lock().unwrap().foyer_miss = true;
            }
        }
        Ok(())
    }
}
