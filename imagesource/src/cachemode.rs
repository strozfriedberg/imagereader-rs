use std::sync::{Arc, atomic::AtomicBool};

pub const DEFAULT_CACHE_MEM_MIB: usize = 256;
pub const DEFAULT_S3_CONCURRENCY: usize = 8;
pub const DEFAULT_CACHE_CHUNK_SIZE: usize = 1024 * 1024;

/// How the foyer cache is structured for a session.
#[derive(Debug, Clone, Default)]
pub enum CacheMode {
    /// Local-file backing: a single memory-only cache.
    #[default]
    SingleMemory,
    /// S3 backing: a dedicated metadata cache plus a content cache. `regular_phase`
    /// starts `false` (metadata phase) and is flipped to `true` by the SIGUSR1 handler.
    DualHybrid {
        content_disk_mib: usize,
        metadata_mem_mib: usize,
        metadata_disk_mib: usize,
        regular_phase: Arc<AtomicBool>,
    },
}
