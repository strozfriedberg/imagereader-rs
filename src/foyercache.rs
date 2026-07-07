use async_trait::async_trait;
use foyer::{
    BlockEngineConfig, DefaultHasher, DeviceBuilder, FsDeviceBuilder, HybridCache,
    HybridCacheBuilder,
};
use foyer_common::code::HashBuilder;
use futures::future::try_join_all;
use std::sync::atomic::{AtomicBool, Ordering};
use std::{
    fmt::Debug,
    future::Future,
    path::Path,
    sync::{Arc, RwLock},
};
use tempfile::TempDir;
use tracing::trace;

use crate::{
    bytessource::BytesSource, cache::Cache, fetch_pool::FetchPool, io_log::ReadTrace,
    placeholdersource::PlaceholderSource,
};

struct MetadataTier<S>
where
    S: HashBuilder + Debug,
{
    cache: Arc<HybridCache<(usize, u64), Vec<u8>, S>>,
    regular_phase: Arc<AtomicBool>,
}

pub struct FoyerCache<S = DefaultHasher>
where
    S: HashBuilder + Debug,
{
    chlen: usize,
    sources: RwLock<Vec<Arc<dyn BytesSource + Send + Sync>>>,
    content: Arc<HybridCache<(usize, u64), Vec<u8>, S>>,
    metadata: Option<MetadataTier<S>>,
    fetch_pool: Arc<FetchPool>,
    _dirs: Vec<TempDir>,
    readahead: usize,
}

fn make_tempdir(base: Option<&Path>) -> std::io::Result<TempDir> {
    match base {
        Some(dir) => tempfile::Builder::new().tempdir_in(dir),
        None => tempfile::tempdir(),
    }
}

async fn build_hybrid(
    mem_capacity: usize,
    disk_size: usize,
    dir: &TempDir,
) -> Result<HybridCache<(usize, u64), Vec<u8>, DefaultHasher>, std::io::Error> {
    let builder = HybridCacheBuilder::new().memory(mem_capacity).storage();
    let builder = if disk_size > 0 {
        let device = FsDeviceBuilder::new(dir.path())
            .with_capacity(disk_size)
            .build()
            .map_err(std::io::Error::other)?;
        builder.with_engine_config(BlockEngineConfig::new(device))
    } else {
        builder
    };
    builder.build().await.map_err(std::io::Error::other)
}

impl FoyerCache<DefaultHasher> {
    pub async fn single_memory(
        chlen: usize,
        mem_capacity: usize,
        readahead: usize,
        s3_concurrency: usize,
        cache_base_dir: Option<&Path>,
    ) -> Result<Self, std::io::Error> {
        let dir = make_tempdir(cache_base_dir)?;
        let content = Arc::new(build_hybrid(mem_capacity, 0, &dir).await?);
        Ok(Self {
            chlen,
            sources: RwLock::new(vec![]),
            content,
            metadata: None,
            fetch_pool: FetchPool::new(s3_concurrency),
            _dirs: vec![dir],
            readahead,
        })
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn dual_hybrid(
        chlen: usize,
        content_mem_mib: usize,
        content_disk_mib: usize,
        metadata_mem_mib: usize,
        metadata_disk_mib: usize,
        readahead: usize,
        s3_concurrency: usize,
        regular_phase: Arc<AtomicBool>,
        cache_base_dir: Option<&Path>,
    ) -> Result<Self, std::io::Error> {
        let content_dir = make_tempdir(cache_base_dir)?;
        let metadata_dir = make_tempdir(cache_base_dir)?;
        let content = Arc::new(
            build_hybrid(
                content_mem_mib,
                content_disk_mib * 1024 * 1024,
                &content_dir,
            )
            .await?,
        );
        let metadata = Arc::new(
            build_hybrid(
                metadata_mem_mib,
                metadata_disk_mib * 1024 * 1024,
                &metadata_dir,
            )
            .await?,
        );
        Ok(Self {
            chlen,
            sources: RwLock::new(vec![]),
            content,
            metadata: Some(MetadataTier {
                cache: metadata,
                regular_phase,
            }),
            fetch_pool: FetchPool::new(s3_concurrency),
            _dirs: vec![content_dir, metadata_dir],
            readahead,
        })
    }
}

fn make_fetch(
    chlen: usize,
    idx: usize,
    choff: u64,
    source: Arc<dyn BytesSource + Send + Sync>,
    end: u64,
    fetch_pool: Arc<FetchPool>,
    trace: Option<Arc<AtomicBool>>,
) -> impl FnOnce() -> std::pin::Pin<Box<dyn Future<Output = Result<Vec<u8>, foyer::Error>> + Send>>
{
    move || {
        let beg = choff;
        let fetch_end = (choff + chlen as u64).min(end);
        Box::pin(async move {
            if let Some(trace) = trace {
                trace.store(true, Ordering::Relaxed);
            }
            fetch_pool
                .run((idx, choff), move || source.read(beg, fetch_end))
                .await
                .map_err(foyer::Error::io_error)
        })
    }
}

fn short_read_error(idx: usize, off: u64, wanted: usize, got: u64) -> std::io::Error {
    std::io::Error::new(
        std::io::ErrorKind::UnexpectedEof,
        format!("source {idx}: short read at offset {off}: filled {got} of {wanted} bytes"),
    )
}

/// Copy the requested `[off, off + buf.len())` range out of a single fetched
/// block `ch` starting at `choff` (the single-block fast path in `Cache::read`).
/// Errors if `ch` came back short (truncated backing store), rather than
/// silently filling `buf` with a shorter, stale-tailed copy.
fn fill_from_block(
    buf: &mut [u8],
    off: u64,
    choff: u64,
    ch: &[u8],
    idx: usize,
) -> Result<(), std::io::Error> {
    let chbeg = (off - choff) as usize;
    let chend = chbeg + buf.len();
    if chend > ch.len() {
        return Err(short_read_error(idx, off, buf.len(), (ch.len() - chbeg) as u64));
    }
    buf.copy_from_slice(&ch[chbeg..chend]);
    Ok(())
}

#[allow(clippy::too_many_arguments)]
async fn route_block(
    chlen: usize,
    idx: usize,
    choff: u64,
    source: Arc<dyn BytesSource + Send + Sync>,
    end: u64,
    content: Arc<HybridCache<(usize, u64), Vec<u8>, DefaultHasher>>,
    metadata: Option<(
        Arc<HybridCache<(usize, u64), Vec<u8>, DefaultHasher>>,
        Arc<AtomicBool>,
    )>,
    fetch_pool: Arc<FetchPool>,
    trace: Option<Arc<AtomicBool>>,
) -> Result<Vec<u8>, std::io::Error> {
    let key = (idx, choff);
    if let Some((md_cache, regular_phase)) = metadata {
        if regular_phase.load(Ordering::Acquire) {
            if let Some(entry) = md_cache.get(&key).await.map_err(std::io::Error::other)? {
                return Ok(entry.value().clone());
            }
            let fetch = make_fetch(chlen, idx, choff, source, end, fetch_pool, trace);
            let entry = content
                .get_or_fetch(&key, fetch)
                .await
                .map_err(std::io::Error::other)?;
            return Ok(entry.value().clone());
        }
        let fetch = make_fetch(chlen, idx, choff, source, end, fetch_pool, trace);
        let entry = md_cache
            .get_or_fetch(&key, fetch)
            .await
            .map_err(std::io::Error::other)?;
        return Ok(entry.value().clone());
    }
    let fetch = make_fetch(chlen, idx, choff, source, end, fetch_pool, trace);
    let entry = content
        .get_or_fetch(&key, fetch)
        .await
        .map_err(std::io::Error::other)?;
    Ok(entry.value().clone())
}

#[async_trait]
impl Cache for FoyerCache<DefaultHasher> {
    async fn read(
        &self,
        idx: usize,
        off: u64,
        buf: &mut [u8],
        trace: &mut ReadTrace,
    ) -> Result<(), std::io::Error> {
        let source = self
            .sources
            .read()
            .expect("sources lock poisoned")
            .get(idx)
            .cloned()
            .ok_or(std::io::Error::other(format!("{idx} out of bounds")))?;
        let end = source.end();
        let chlen = self.chlen as u64;

        let csbeg = (off / chlen) * chlen;
        let csend = off + buf.len() as u64;
        let (rabeg, raend) = readahead_block_range(csend, chlen, self.readahead, end);

        let md = self
            .metadata
            .as_ref()
            .map(|m| (m.cache.clone(), m.regular_phase.clone()));

        // The overwhelming majority of calls (struct-field-sized reads during
        // header/grain-table parsing) need exactly one block. Skip the
        // trace-cell allocation and try_join_all/iterator machinery for that
        // case instead of paying per-call async-Mutex + Vec overhead on
        // what's usually a cache hit.
        let mut demand_offs = (csbeg..csend).step_by(self.chlen);
        let first = demand_offs.next();
        let second = demand_offs.next();

        let miss = match (first, second) {
            (Some(choff), None) => {
                let trace_cell = Arc::new(AtomicBool::new(false));
                let ch = route_block(
                    self.chlen,
                    idx,
                    choff,
                    source.clone(),
                    end,
                    self.content.clone(),
                    md.clone(),
                    self.fetch_pool.clone(),
                    Some(trace_cell.clone()),
                )
                .await?;
                fill_from_block(buf, off, choff, &ch, idx)?;
                trace!("fetched {idx} [{choff},{})", choff + ch.len() as u64);
                trace_cell.load(Ordering::Relaxed)
            }
            _ => {
                let trace_cell = Arc::new(AtomicBool::new(false));
                let demand = (csbeg..csend).step_by(self.chlen).map(|choff| {
                    route_block(
                        self.chlen,
                        idx,
                        choff,
                        source.clone(),
                        end,
                        self.content.clone(),
                        md.clone(),
                        self.fetch_pool.clone(),
                        Some(trace_cell.clone()),
                    )
                });
                let chunks = try_join_all(demand).await?;

                let mut bbeg = 0u64;
                for (choff, ch) in (csbeg..csend).step_by(self.chlen).zip(chunks) {
                    trace!("fetched {idx} [{choff},{})", choff + ch.len() as u64);

                    // A short earlier block means this block's math would underflow.
                    if off + bbeg < choff {
                        return Err(short_read_error(idx, off, buf.len(), bbeg));
                    }

                    let chbeg = (off + bbeg) - choff;
                    let chend = (chbeg + (buf.len() as u64 - bbeg)).min(ch.len() as u64);
                    if chend < chbeg {
                        return Err(short_read_error(idx, off, buf.len(), bbeg));
                    }
                    let bend = bbeg + (chend - chbeg);

                    buf[bbeg as usize..bend as usize]
                        .copy_from_slice(&ch[chbeg as usize..chend as usize]);

                    trace!("filled [{},{})", off + bbeg, off + bend);
                    bbeg = bend;
                }

                if bbeg != buf.len() as u64 {
                    return Err(short_read_error(idx, off, buf.len(), bbeg));
                }

                trace_cell.load(Ordering::Relaxed)
            }
        };
        trace.foyer_miss = miss;

        for choff in (rabeg..raend).step_by(self.chlen) {
            let fut = route_block(
                self.chlen,
                idx,
                choff,
                source.clone(),
                end,
                self.content.clone(),
                md.clone(),
                self.fetch_pool.clone(),
                None,
            );
            tokio::spawn(async move {
                let _ = fut.await;
            });
        }

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

/// Byte range `[beg, end)` of foyer blocks to prefetch after a read ending at `csend`.
pub(crate) fn readahead_block_range(
    csend: u64,
    chlen: u64,
    readahead: usize,
    end: u64,
) -> (u64, u64) {
    let beg = csend.div_ceil(chlen) * chlen;
    let raend = (beg + (readahead as u64) * chlen).min(end);
    (beg, raend)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bytessource::BytesSource;
    use crate::filesource::FileSource;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};

    const MIB: u64 = 1024 * 1024;

    fn test_source() -> Box<dyn BytesSource + Send + Sync> {
        let path = "data/monolithicFlat-flat.vmdk";
        let len = std::fs::metadata(path).unwrap().len();
        Box::new(FileSource {
            path: path.into(),
            len,
        })
    }

    #[test]
    fn readahead_starts_at_next_full_block() {
        let (beg, end) = readahead_block_range(4096, MIB, 4, 10 * MIB);
        assert_eq!(beg, MIB);
        assert_eq!(end, 5 * MIB);
    }

    #[test]
    fn readahead_zero_is_empty_range() {
        let (beg, end) = readahead_block_range(4096, MIB, 0, 10 * MIB);
        assert_eq!(beg, MIB);
        assert_eq!(end, MIB);
    }

    #[test]
    fn readahead_clamps_to_source_end() {
        let (beg, end) = readahead_block_range(MIB, MIB, 8, 3 * MIB);
        assert_eq!(beg, MIB);
        assert_eq!(end, 3 * MIB);
    }

    #[tokio::test]
    async fn metadata_phase_routes_to_metadata_cache_then_protects() {
        const CHUNK: usize = 64 * 1024;
        const CHUNK64: u64 = CHUNK as u64;

        let regular = Arc::new(AtomicBool::new(false));
        let cache = FoyerCache::dual_hybrid(CHUNK, 64, 0, 64, 0, 0, 4, regular.clone(), None)
            .await
            .unwrap();
        cache.add_source(0, test_source());

        // Metadata phase: block 0 (offset 0)
        let mut a = vec![0u8; 4096];
        let mut t = ReadTrace::default();
        cache.read(0, 0, &mut a, &mut t).await.unwrap();
        assert!(cache.metadata.as_ref().unwrap().cache.contains(&(0, 0)));
        assert!(!cache.content.contains(&(0, 0)));

        // Switch to regular phase and read block 1 (offset CHUNK)
        regular.store(true, Ordering::Relaxed);
        let mut b = vec![0u8; 4096];
        cache.read(0, CHUNK64, &mut b, &mut t).await.unwrap();
        assert!(cache.content.contains(&(0, CHUNK64)));
        assert!(
            !cache
                .metadata
                .as_ref()
                .unwrap()
                .cache
                .contains(&(0, CHUNK64))
        );

        // Block 0 was read during metadata phase — stays in metadata cache
        cache.read(0, 0, &mut a, &mut t).await.unwrap();
        assert!(cache.metadata.as_ref().unwrap().cache.contains(&(0, 0)));
    }

    #[tokio::test]
    async fn dual_hybrid_creates_tempdirs_under_custom_base() {
        let base = tempfile::tempdir().unwrap();
        let before: Vec<_> = std::fs::read_dir(base.path()).unwrap().collect();
        assert_eq!(before.len(), 0);

        let regular = Arc::new(AtomicBool::new(false));
        let _cache = FoyerCache::dual_hybrid(
            64 * 1024,
            64,
            1,
            64,
            1,
            0,
            4,
            regular,
            Some(base.path()),
        )
        .await
        .unwrap();

        let after: Vec<_> = std::fs::read_dir(base.path()).unwrap().collect();
        assert_eq!(after.len(), 2, "expected content_dir and metadata_dir under the custom base");
    }
}
