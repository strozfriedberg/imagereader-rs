use async_trait::async_trait;
use foyer::{
    BlockEngineConfig, DefaultHasher, DeviceBuilder, FsDeviceBuilder, HybridCache,
    HybridCacheBuilder, HybridCacheEntry,
};
use foyer_common::code::HashBuilder;
use futures::future::try_join_all;
use std::sync::atomic::{AtomicBool, Ordering};
use std::{fmt::Debug, future::Future, path::Path, sync::Arc};
use tempfile::TempDir;
use tokio::sync::OwnedSemaphorePermit;
use tracing::trace;

use crate::{
    bytessource::BytesSource,
    cache::Cache,
    fetch_limit::FetchLimiter,
    io_log::{IoLog, ReadTrace},
    source_slot::SourceSlots,
};

/// A block-content cache keyed by `(source/extent index, segment-file byte offset)`.
type BlockCache = HybridCache<(usize, u64), Vec<u8>, DefaultHasher>;

/// A refcounted handle to a cached block, handed back by `route_block`.
///
/// Deliberately not a `Vec<u8>`: a block is `chlen` bytes (1 MiB by default), so
/// copying one out of the cache to serve a 4 KiB read would move 256x more bytes
/// than the caller asked for -- on every read, including cache hits. Foyer's
/// entries are already refcounted; this keeps that property instead of throwing
/// it away. Derefs to the block's bytes.
type BlockEntry = HybridCacheEntry<(usize, u64), Vec<u8>, DefaultHasher>;

/// A boxed future returned by a fetch closure passed to `FetchLimiter::run`.
type FetchFuture = std::pin::Pin<Box<dyn Future<Output = Result<Vec<u8>, foyer::Error>> + Send>>;

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
    sources: SourceSlots,
    content: Arc<HybridCache<(usize, u64), Vec<u8>, S>>,
    metadata: Option<MetadataTier<S>>,
    fetch_limit: Arc<FetchLimiter>,
    _dirs: Vec<TempDir>,
    readahead: usize,
    io_log: Option<Arc<IoLog>>,
}

impl<S> FoyerCache<S>
where
    S: HashBuilder + Debug,
{
    /// Attach a trace log, so prefetch decisions show up in the JSONL trace.
    pub fn with_io_log(mut self, io_log: Option<Arc<IoLog>>) -> Self {
        self.io_log = io_log;
        self
    }
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
) -> Result<BlockCache, std::io::Error> {
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
            sources: SourceSlots::default(),
            content,
            metadata: None,
            fetch_limit: FetchLimiter::new(s3_concurrency),
            _dirs: vec![dir],
            readahead,
            io_log: None,
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
            sources: SourceSlots::default(),
            content,
            metadata: Some(MetadataTier {
                cache: metadata,
                regular_phase,
            }),
            fetch_limit: FetchLimiter::new(s3_concurrency),
            _dirs: vec![content_dir, metadata_dir],
            readahead,
            io_log: None,
        })
    }
}

fn make_fetch(
    chlen: usize,
    choff: u64,
    source: Arc<dyn BytesSource + Send + Sync>,
    end: u64,
    fetch_limit: Arc<FetchLimiter>,
    permit: Option<OwnedSemaphorePermit>,
    trace: Option<Arc<AtomicBool>>,
) -> impl FnOnce() -> FetchFuture {
    move || {
        let beg = choff;
        let fetch_end = (choff + chlen as u64).min(end);
        Box::pin(async move {
            if let Some(trace) = trace {
                trace.store(true, Ordering::Relaxed);
            }
            let result = match permit {
                // Readahead already took a permit with `try_permit`; hold it for
                // the duration of the read instead of queueing for a second one.
                Some(permit) => {
                    let _permit = permit;
                    source.read(beg, fetch_end).await
                }
                None => fetch_limit.run(move || source.read(beg, fetch_end)).await,
            };
            result.map_err(foyer::Error::io_error)
        })
    }
}

pub(crate) fn short_read_error(idx: usize, off: u64, wanted: usize, got: u64) -> std::io::Error {
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
        // `chbeg` can itself land past the block's tail if the block came back
        // very short, so clamp with saturating_sub rather than underflowing.
        return Err(short_read_error(
            idx,
            off,
            buf.len(),
            ch.len().saturating_sub(chbeg) as u64,
        ));
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
    content: Arc<BlockCache>,
    metadata: Option<(Arc<BlockCache>, Arc<AtomicBool>)>,
    fetch_limit: Arc<FetchLimiter>,
    permit: Option<OwnedSemaphorePermit>,
    trace: Option<Arc<AtomicBool>>,
) -> Result<BlockEntry, std::io::Error> {
    let key = (idx, choff);
    if let Some((md_cache, regular_phase)) = metadata {
        if regular_phase.load(Ordering::Acquire) {
            if let Some(entry) = md_cache.get(&key).await.map_err(std::io::Error::other)? {
                return Ok(entry);
            }
            let fetch = make_fetch(chlen, choff, source, end, fetch_limit, permit, trace);
            return content
                .get_or_fetch(&key, fetch)
                .await
                .map_err(std::io::Error::other);
        }
        let fetch = make_fetch(chlen, choff, source, end, fetch_limit, permit, trace);
        return md_cache
            .get_or_fetch(&key, fetch)
            .await
            .map_err(std::io::Error::other);
    }
    let fetch = make_fetch(chlen, choff, source, end, fetch_limit, permit, trace);
    content
        .get_or_fetch(&key, fetch)
        .await
        .map_err(std::io::Error::other)
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
        let source = self.sources.get(idx)?;
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
                    self.fetch_limit.clone(),
                    None,
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
                        self.fetch_limit.clone(),
                        None,
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

        let mut prefetched = vec![];

        for choff in (rabeg..raend).step_by(self.chlen) {
            let key = (idx, choff);

            // Don't spawn work for blocks we already hold. Without this, a
            // sequential scan re-spawns a prefetch for every resident block on
            // every read, so task churn tracks read rate rather than miss rate.
            if self.content.contains(&key)
                || md
                    .as_ref()
                    .is_some_and(|(md_cache, _)| md_cache.contains(&key))
            {
                continue;
            }

            // Speculation gets only spare capacity. A prefetch that would have
            // to queue for a permit is dropped instead: it must never make a
            // demand read -- one a client is actually blocked on -- wait behind
            // blocks nobody asked for.
            let Some(permit) = self.fetch_limit.try_permit() else {
                break;
            };

            let fut = route_block(
                self.chlen,
                idx,
                choff,
                source.clone(),
                end,
                self.content.clone(),
                md.clone(),
                self.fetch_limit.clone(),
                Some(permit),
                None,
            );
            tokio::spawn(async move {
                let _ = fut.await;
            });

            prefetched.push(choff);
        }

        // Only the blocks we actually enqueued: resident ones and ones dropped
        // for lack of spare capacity are deliberately not counted.
        if let Some(log) = &self.io_log {
            log.log_prefetch(idx, csbeg, &prefetched);
        }

        Ok(())
    }

    fn end(&self, idx: usize) -> Result<u64, std::io::Error> {
        self.sources.get(idx).map(|src| src.end())
    }

    fn add_source(&self, idx: usize, src: Box<dyn BytesSource + Send + Sync>) {
        self.sources.set(idx, src);
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
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::time::Duration;

    const MIB: u64 = 1024 * 1024;

    /// A 1 MiB patterned temp file; the TempDir keeps it alive for the test.
    fn test_source() -> (tempfile::TempDir, Box<dyn BytesSource + Send + Sync>) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("source.bin");
        let data: Vec<u8> = (0..MIB).map(|i| (i % 251) as u8).collect();
        std::fs::write(&path, &data).unwrap();
        let src = Box::new(FileSource::open(&path).unwrap());
        (dir, src)
    }

    /// A block that came back short enough that the requested offset lands past
    /// its tail must yield an error, not underflow `ch.len() - chbeg`.
    #[test]
    fn fill_from_block_short_block_errors_without_underflow() {
        let mut buf = [0u8; 16];
        // block starts at choff=0 but is only 4 bytes; we want [8, 24).
        let ch = [1u8, 2, 3, 4];
        let err = fill_from_block(&mut buf, 8, 0, &ch, 0).unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::UnexpectedEof);
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
        let (_dir, src) = test_source();
        cache.add_source(0, src);

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
        let _cache =
            FoyerCache::dual_hybrid(64 * 1024, 64, 1, 64, 1, 0, 4, regular, Some(base.path()))
                .await
                .unwrap();

        let after: Vec<_> = std::fs::read_dir(base.path()).unwrap().collect();
        assert_eq!(
            after.len(),
            2,
            "expected content_dir and metadata_dir under the custom base"
        );
    }

    use futures::FutureExt;

    /// Simulates a truncated backing store: returns half the requested range.
    struct ShortSource {
        len: u64,
    }

    impl BytesSource for ShortSource {
        fn read(
            &self,
            beg: u64,
            end: u64,
        ) -> futures::future::BoxFuture<'static, Result<Vec<u8>, std::io::Error>> {
            async move { Ok(vec![0u8; ((end - beg) / 2) as usize]) }.boxed()
        }

        fn end(&self) -> u64 {
            self.len
        }
    }

    /// Counts how many times the backing store is actually hit.
    struct CountingSource {
        len: u64,
        reads: Arc<AtomicUsize>,
    }

    impl BytesSource for CountingSource {
        fn read(
            &self,
            beg: u64,
            end: u64,
        ) -> futures::future::BoxFuture<'static, Result<Vec<u8>, std::io::Error>> {
            let reads = self.reads.clone();
            async move {
                reads.fetch_add(1, Ordering::SeqCst);
                // Wide enough that the other readers pile up behind this one.
                tokio::time::sleep(Duration::from_millis(50)).await;
                Ok(vec![0u8; (end - beg) as usize])
            }
            .boxed()
        }

        fn end(&self) -> u64 {
            self.len
        }
    }

    /// We deliberately do not dedupe fetches ourselves -- foyer's `get_or_fetch`
    /// coalesces concurrent misses for the same key. This pins that down: if
    /// foyer ever stopped single-flighting, we'd silently start issuing N S3
    /// GETs for one block.
    #[tokio::test]
    async fn concurrent_misses_of_one_block_hit_the_source_once() {
        const CHUNK: usize = 64 * 1024;
        let reads = Arc::new(AtomicUsize::new(0));

        let cache = Arc::new(
            FoyerCache::single_memory(CHUNK, 16, 0, 8, None)
                .await
                .unwrap(),
        );
        cache.add_source(
            0,
            Box::new(CountingSource {
                len: MIB,
                reads: reads.clone(),
            }),
        );

        // Eight readers, all wanting different bytes of the *same* block.
        let handles: Vec<_> = (0..8u64)
            .map(|i| {
                let cache = cache.clone();
                tokio::spawn(async move {
                    let mut buf = vec![0u8; 512];
                    let mut t = ReadTrace::default();
                    cache.read(0, i * 512, &mut buf, &mut t).await.unwrap();
                })
            })
            .collect();
        for h in handles {
            h.await.unwrap();
        }

        assert_eq!(
            reads.load(Ordering::SeqCst),
            1,
            "concurrent misses of one block must coalesce into a single fetch"
        );
    }

    /// Readahead must still do its job: the blocks after the one we read get
    /// pulled in, so a following sequential read is served without a fetch.
    #[tokio::test]
    async fn readahead_warms_following_blocks() {
        const CHUNK: usize = 64 * 1024;
        const CHUNK64: u64 = CHUNK as u64;
        let reads = Arc::new(AtomicUsize::new(0));

        // Plenty of spare capacity (8 permits) for a readahead depth of 2.
        let cache = FoyerCache::single_memory(CHUNK, 16, 2, 8, None)
            .await
            .unwrap();
        cache.add_source(
            0,
            Box::new(CountingSource {
                len: MIB,
                reads: reads.clone(),
            }),
        );

        let mut buf = vec![0u8; 512];
        let mut t = ReadTrace::default();
        cache.read(0, 0, &mut buf, &mut t).await.unwrap();

        // Let the two spawned prefetches (blocks 1 and 2) land.
        tokio::time::sleep(Duration::from_millis(300)).await;
        assert_eq!(
            reads.load(Ordering::SeqCst),
            3,
            "expected the demand block plus 2 prefetched blocks"
        );

        // Block 1 was prefetched, so reading it is a cache hit, not a fetch.
        cache.read(0, CHUNK64, &mut buf, &mut t).await.unwrap();
        assert!(!t.foyer_miss, "prefetched block must be served from cache");

        // That read slides the window to blocks 2 and 3. Block 2 is already
        // resident and must be skipped, so only block 3 is newly fetched.
        tokio::time::sleep(Duration::from_millis(300)).await;
        assert_eq!(
            reads.load(Ordering::SeqCst),
            4,
            "only the one block outside the cache should be prefetched"
        );

        // Re-reading block 0 prefetches nothing: every block in its window is
        // already resident. Without suppression this would re-spawn fetches.
        cache.read(0, 0, &mut buf, &mut t).await.unwrap();
        tokio::time::sleep(Duration::from_millis(300)).await;
        assert_eq!(
            reads.load(Ordering::SeqCst),
            4,
            "resident blocks must not be prefetched again"
        );
    }

    /// Speculation only gets spare capacity. With a single permit, the demand
    /// read consumes it and at most one prefetch can claim it afterwards -- the
    /// rest are dropped rather than queued ahead of future demand reads.
    #[tokio::test]
    async fn readahead_takes_only_spare_capacity() {
        const CHUNK: usize = 64 * 1024;
        let reads = Arc::new(AtomicUsize::new(0));

        // One permit, but a readahead depth of 4.
        let cache = FoyerCache::single_memory(CHUNK, 16, 4, 1, None)
            .await
            .unwrap();
        cache.add_source(
            0,
            Box::new(CountingSource {
                len: MIB,
                reads: reads.clone(),
            }),
        );

        let mut buf = vec![0u8; 512];
        let mut t = ReadTrace::default();
        cache.read(0, 0, &mut buf, &mut t).await.unwrap();

        // Long enough that all 4 prefetches would have completed if they had
        // queued for the permit instead of being dropped.
        tokio::time::sleep(Duration::from_millis(500)).await;

        assert_eq!(
            reads.load(Ordering::SeqCst),
            2,
            "the demand block plus at most one prefetch holding the only permit; \
             queued-up prefetches would make this 5"
        );
    }

    /// The trace records the prefetches actually issued -- not the ones skipped
    /// as resident or dropped for lack of spare capacity.
    #[tokio::test]
    async fn prefetch_trace_counts_only_enqueued_blocks() {
        const CHUNK: usize = 64 * 1024;
        const CHUNK64: u64 = CHUNK as u64;

        let dir = tempfile::tempdir().unwrap();
        let log_path = dir.path().join("io.jsonl");
        let io_log = IoLog::open(&log_path).unwrap();
        io_log.begin_serving().unwrap();

        let reads = Arc::new(AtomicUsize::new(0));
        let cache = FoyerCache::single_memory(CHUNK, 16, 2, 8, None)
            .await
            .unwrap()
            .with_io_log(Some(io_log.clone()));
        cache.add_source(
            0,
            Box::new(CountingSource {
                len: MIB,
                reads: reads.clone(),
            }),
        );

        let mut buf = vec![0u8; 512];
        let mut t = ReadTrace::default();
        cache.read(0, 0, &mut buf, &mut t).await.unwrap();
        tokio::time::sleep(Duration::from_millis(300)).await;

        let lines = std::fs::read_to_string(&log_path).unwrap();
        let prefetches: Vec<_> = lines
            .lines()
            .filter(|l| l.contains(r#""kind":"prefetch""#))
            .collect();

        assert_eq!(prefetches.len(), 1, "one read, one prefetch record");
        assert!(
            prefetches[0].contains(r#""count":2"#),
            "expected 2 enqueued blocks, got: {}",
            prefetches[0]
        );
        assert!(
            prefetches[0].contains(&format!("[{},{}]", CHUNK64, 2 * CHUNK64)),
            "expected the two blocks after the demand block, got: {}",
            prefetches[0]
        );

        // Re-reading block 0 enqueues nothing: its whole window is resident.
        cache.read(0, 0, &mut buf, &mut t).await.unwrap();
        tokio::time::sleep(Duration::from_millis(300)).await;

        let lines = std::fs::read_to_string(&log_path).unwrap();
        let prefetches = lines
            .lines()
            .filter(|l| l.contains(r#""kind":"prefetch""#))
            .count();
        assert_eq!(
            prefetches, 1,
            "a read that enqueues no prefetches must not emit a record"
        );
    }

    struct SlowSource {
        len: u64,
        active: Arc<AtomicUsize>,
        max_active: Arc<AtomicUsize>,
    }

    impl BytesSource for SlowSource {
        fn read(
            &self,
            beg: u64,
            end: u64,
        ) -> futures::future::BoxFuture<'static, Result<Vec<u8>, std::io::Error>> {
            let active = self.active.clone();
            let max_active = self.max_active.clone();
            async move {
                let now = active.fetch_add(1, Ordering::SeqCst) + 1;
                max_active.fetch_max(now, Ordering::SeqCst);
                tokio::time::sleep(Duration::from_millis(100)).await;
                active.fetch_sub(1, Ordering::SeqCst);
                Ok(vec![0u8; (end - beg) as usize])
            }
            .boxed()
        }

        fn end(&self) -> u64 {
            self.len
        }
    }

    #[tokio::test]
    async fn concurrent_reads_reach_the_source_in_parallel() {
        const CHUNK: usize = 64 * 1024;
        let active = Arc::new(AtomicUsize::new(0));
        let max_active = Arc::new(AtomicUsize::new(0));

        let cache = Arc::new(
            FoyerCache::single_memory(CHUNK, 16, 0, 8, None)
                .await
                .unwrap(),
        );
        cache.add_source(
            0,
            Box::new(SlowSource {
                len: 1024 * 1024,
                active: active.clone(),
                max_active: max_active.clone(),
            }),
        );

        // Four reads of four distinct blocks through the shared handle.
        let handles: Vec<_> = (0..4u64)
            .map(|i| {
                let cache = cache.clone();
                tokio::spawn(async move {
                    let mut buf = vec![0u8; 4096];
                    let mut t = ReadTrace::default();
                    cache
                        .read(0, i * CHUNK as u64, &mut buf, &mut t)
                        .await
                        .unwrap();
                })
            })
            .collect();
        for h in handles {
            h.await.unwrap();
        }

        let max = max_active.load(Ordering::SeqCst);
        assert!(
            max >= 2,
            "block fetches never overlapped (max in-flight: {max})"
        );
    }

    #[tokio::test]
    async fn short_block_from_source_is_an_error_not_silent_zeros() {
        const CHUNK: usize = 64 * 1024;
        let cache = FoyerCache::single_memory(CHUNK, 16, 0, 4, None)
            .await
            .unwrap();
        cache.add_source(0, Box::new(ShortSource { len: 1024 * 1024 }));

        // Request exactly one full block; the source returns only half of it.
        let mut buf = vec![0u8; CHUNK];
        let mut t = ReadTrace::default();
        let err = cache.read(0, 0, &mut buf, &mut t).await.unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::UnexpectedEof);

        // A short block in the middle of a multi-block read must also error
        // (this is the case that underflows `(off + bbeg) - choff` today).
        let mut buf = vec![0u8; CHUNK * 2];
        let err = cache.read(0, 0, &mut buf, &mut t).await.unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::UnexpectedEof);
    }
}
