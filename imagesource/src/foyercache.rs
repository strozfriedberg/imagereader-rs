use async_trait::async_trait;
use foyer::{
    BlockEngineConfig, DefaultHasher, DeviceBuilder, FsDeviceBuilder, HybridCache,
    HybridCacheBuilder, HybridCacheEntry,
};
use futures::future::{BoxFuture, FutureExt, Shared, try_join_all};
use std::collections::HashMap;
use std::panic::AssertUnwindSafe;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::{future::Future, path::Path, sync::Arc};
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

/// The bytes of one aligned fetch group, shared by every block waiting on it.
/// `io::Error` is not `Clone`, which `Shared` requires, hence the `Arc`.
type GroupResult = Result<Arc<Vec<u8>>, Arc<std::io::Error>>;
type GroupFuture = Shared<BoxFuture<'static, GroupResult>>;

/// The aligned group a coalesced fetch covers: always a whole number of blocks.
///
/// `fetch_size` is a user-supplied byte count and need not be a multiple of the
/// block size. It has to be rounded *down* to one, because `gbeg` is derived from
/// it and the cache keys live on the `chlen` grid. A group that starts mid-block
/// emits no `boff` equal to the demanded `choff`, so the fetch returns no demanded
/// block and the read fails with a bogus short-read error -- permanently, for
/// every block whose group start is misaligned -- while the siblings it did insert
/// land under keys no lookup will ever use.
fn group_size(fetch_size: usize, chlen: u64) -> u64 {
    ((fetch_size as u64) / chlen).max(1) * chlen
}

/// The fetch size that will actually be used for a given block size, so callers
/// can report the effective value rather than the one that was asked for.
pub fn aligned_fetch_size(fetch_size: usize, block_size: usize) -> usize {
    group_size(fetch_size, normalize_block_size(block_size) as u64) as usize
}

/// Dedups concurrent fetches of the same aligned group.
///
/// foyer single-flights per *key*, and a group is not a key, so without this every
/// block of a group that misses concurrently issues its own GET for the whole
/// group. Two ordinary paths do exactly that: a multi-block demand read fans its
/// blocks out through `try_join_all`, and the readahead loop spawns a task per
/// block without waiting for the previous one to land. Both multiply round trips
/// and bytes by up to `fetch_size / chlen` -- precisely inverting what coalescing
/// exists to do.
#[derive(Default)]
struct GroupLatch {
    inflight: Mutex<HashMap<(usize, u64), GroupFuture>>,
}

/// Read `[gbeg, gend)` from the backing store, respecting the fetch limiter.
///
/// A readahead caller already took a permit with `try_permit` and holds it for the
/// duration of the read; a demand caller queues for one.
async fn fetch_range(
    gbeg: u64,
    gend: u64,
    source: Arc<dyn BytesSource + Send + Sync>,
    fetch_limit: Arc<FetchLimiter>,
    permit: Option<OwnedSemaphorePermit>,
) -> Result<Vec<u8>, std::io::Error> {
    match permit {
        Some(permit) => {
            let _permit = permit;
            source.read(gbeg, gend).await
        }
        None => fetch_limit.run(move || source.read(gbeg, gend)).await,
    }
}

impl GroupLatch {
    /// Read `[gbeg, gend)` of source `idx`, joining an already in-flight fetch of
    /// the same group instead of issuing a second one.
    ///
    /// The returned flag is true only for the caller that created the fetch. Only
    /// that caller inserts the group's sibling blocks: joiners would clone and
    /// insert exactly the same bytes under exactly the same keys.
    async fn fetch(
        self: &Arc<Self>,
        idx: usize,
        gbeg: u64,
        gend: u64,
        source: Arc<dyn BytesSource + Send + Sync>,
        fetch_limit: Arc<FetchLimiter>,
        permit: Option<OwnedSemaphorePermit>,
    ) -> (GroupResult, bool) {
        let key = (idx, gbeg);
        let (fut, owner) = {
            // The map holds no invariant a panic could break, so recover from
            // poisoning rather than turning one panic into a dead cache.
            let mut inflight = self.inflight.lock().unwrap_or_else(|e| e.into_inner());
            match inflight.get(&key) {
                Some(fut) => {
                    // Joining, not reading: hand the permit back now instead of
                    // holding capacity for a GET we are not going to issue.
                    drop(permit);
                    (fut.clone(), false)
                }
                None => {
                    let latch = self.clone();
                    let fut = async move {
                        // Caught, not propagated, because the retire below has to
                        // run. An unwind would skip it and leave a `Shared` whose
                        // inner future has already panicked installed under this
                        // key -- every later miss in the group would then join the
                        // dead future instead of retrying, wedging `fetch_size`
                        // bytes of the image for the reader's lifetime. As an
                        // ordinary error it fails this read and the next one
                        // retries, which is how a source `Err` already behaves.
                        let bytes =
                            AssertUnwindSafe(fetch_range(gbeg, gend, source, fetch_limit, permit))
                                .catch_unwind()
                                .await
                                .unwrap_or_else(|_| {
                                    Err(std::io::Error::other(format!(
                                        "source {idx}: panicked reading [{gbeg},{gend})"
                                    )))
                                });
                        // Retire the entry before waking the waiters, so the next
                        // miss on this group starts a fresh fetch rather than
                        // joining a finished one.
                        latch
                            .inflight
                            .lock()
                            .unwrap_or_else(|e| e.into_inner())
                            .remove(&key);
                        bytes.map(Arc::new).map_err(Arc::new)
                    }
                    .boxed()
                    .shared();
                    inflight.insert(key, fut.clone());
                    (fut, true)
                }
            }
        };
        // Bound to a `let` so the `Shared` handle drops at the end of this
        // statement rather than living until the function returns, releasing the
        // copy it caches for late joiners as soon as we are done with it.
        let result = fut.await;
        (result, owner)
    }
}

struct MetadataTier {
    cache: Arc<BlockCache>,
    regular_phase: Arc<AtomicBool>,
}

/// Everything a block lookup needs apart from the block itself: the caches, the
/// fetch limiter, the group latch and the sizes that shape a fetch.
///
/// Held behind one `Arc` so the demand path can borrow it and a spawned prefetch
/// can take a single owned handle, rather than cloning half a dozen `Arc`s --
/// each a contended refcount write on a line every other reader also wants.
struct Tiers {
    chlen: usize,
    /// Bytes pulled from the backing store per miss. Always a whole multiple of
    /// `chlen`; when larger, one GET fills several cache blocks. See `make_fetch`.
    fetch_size: usize,
    content: Arc<BlockCache>,
    metadata: Option<MetadataTier>,
    fetch_limit: Arc<FetchLimiter>,
    /// Joins concurrent misses on one fetch group into a single GET.
    groups: Arc<GroupLatch>,
    io_log: Option<Arc<IoLog>>,
}

pub struct FoyerCache {
    tiers: Arc<Tiers>,
    sources: SourceSlots,
    _dirs: Vec<TempDir>,
    readahead: usize,
}

impl FoyerCache {
    /// Attach a trace log, so prefetch decisions show up in the JSONL trace.
    pub fn with_io_log(mut self, io_log: Option<Arc<IoLog>>) -> Self {
        Arc::get_mut(&mut self.tiers)
            .expect("with_io_log is called before any read, so nothing else holds the tiers")
            .io_log = io_log;
        self
    }
}

fn make_tempdir(base: Option<&Path>) -> std::io::Result<TempDir> {
    match base {
        Some(dir) => tempfile::Builder::new().tempdir_in(dir),
        None => tempfile::tempdir(),
    }
}

/// Memory capacity in MiB, converted to the entry count foyer actually wants.
///
/// foyer's default weighter is `|_, _| 1`, so `.memory(n)` caps the cache at `n`
/// *entries*, not `n` bytes; a byte budget has to be divided by the block size.
fn mem_entries(mem_mib: usize, block_size: usize) -> usize {
    // A zero block size is a caller bug, not a request for 67 million entries.
    // Fall back to the 1 MiB default rather than dividing by a clamped 1 byte.
    let block = if block_size == 0 {
        1024 * 1024
    } else {
        block_size
    };
    ((mem_mib * 1024 * 1024) / block).max(1)
}

/// A usable block size. Zero is a caller bug, and left alone it would panic in
/// `step_by` and divide by zero in `group_size`; fall back to the 1 MiB default,
/// matching what [`mem_entries`] does with the same input.
fn normalize_block_size(chlen: usize) -> usize {
    if chlen == 0 { 1024 * 1024 } else { chlen }
}

async fn build_hybrid(
    mem_mib: usize,
    block_size: usize,
    disk_size: usize,
    dir: &TempDir,
) -> Result<BlockCache, std::io::Error> {
    let builder = HybridCacheBuilder::new()
        .memory(mem_entries(mem_mib, block_size))
        .storage();
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

impl FoyerCache {
    pub async fn single_memory(
        chlen: usize,
        fetch_size: usize,
        mem_capacity: usize,
        readahead: usize,
        s3_concurrency: usize,
        cache_base_dir: Option<&Path>,
    ) -> Result<Self, std::io::Error> {
        let chlen = normalize_block_size(chlen);
        let dir = make_tempdir(cache_base_dir)?;
        let content = Arc::new(build_hybrid(mem_capacity, chlen, 0, &dir).await?);
        Ok(Self {
            tiers: Arc::new(Tiers {
                chlen,
                fetch_size: group_size(fetch_size, chlen as u64) as usize,
                content,
                metadata: None,
                fetch_limit: FetchLimiter::new(s3_concurrency),
                groups: Arc::new(GroupLatch::default()),
                io_log: None,
            }),
            sources: SourceSlots::default(),
            _dirs: vec![dir],
            readahead,
        })
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn dual_hybrid(
        chlen: usize,
        fetch_size: usize,
        content_mem_mib: usize,
        content_disk_mib: usize,
        metadata_mem_mib: usize,
        metadata_disk_mib: usize,
        readahead: usize,
        s3_concurrency: usize,
        regular_phase: Arc<AtomicBool>,
        cache_base_dir: Option<&Path>,
    ) -> Result<Self, std::io::Error> {
        let chlen = normalize_block_size(chlen);
        let content_dir = make_tempdir(cache_base_dir)?;
        let metadata_dir = make_tempdir(cache_base_dir)?;
        let content = Arc::new(
            build_hybrid(
                content_mem_mib,
                chlen,
                content_disk_mib * 1024 * 1024,
                &content_dir,
            )
            .await?,
        );
        let metadata = Arc::new(
            build_hybrid(
                metadata_mem_mib,
                chlen,
                metadata_disk_mib * 1024 * 1024,
                &metadata_dir,
            )
            .await?,
        );
        Ok(Self {
            tiers: Arc::new(Tiers {
                chlen,
                fetch_size: group_size(fetch_size, chlen as u64) as usize,
                content,
                metadata: Some(MetadataTier {
                    cache: metadata,
                    regular_phase,
                }),
                fetch_limit: FetchLimiter::new(s3_concurrency),
                groups: Arc::new(GroupLatch::default()),
                io_log: None,
            }),
            sources: SourceSlots::default(),
            _dirs: vec![content_dir, metadata_dir],
            readahead,
        })
    }
}

/// Fetch coarse, cache fine.
///
/// The size we should *fetch* and the size we should *cache* want opposite
/// things, and using one number for both forces a bad trade.
///
/// Fetching wants to be big. A range GET against S3 costs almost entirely fixed
/// latency -- measured on a real image, 1 MiB took 221 ms and 16 MiB took 153 ms
/// -- so the runtime tracks the number of round trips, not the bytes. Caching
/// wants to be small: a scattered 200 KB index read should not pin 8 MiB of
/// mostly-junk in a cache that is under pressure, because on a large image the
/// metadata working set already fills it, and every eviction it causes comes back
/// as another 140 ms round trip.
///
/// So: pull the whole aligned `fetch_size` group in one GET, then cut it into
/// `chlen` blocks and insert each as its own cache entry. The demanded block is
/// returned (foyer's `get_or_fetch` inserts that one). The siblings are
/// speculative, and being separate entries, an LRU evicts them *first* -- they
/// are never touched, while the demanded blocks keep getting hit. Under a full
/// cache this degrades toward one GET per block instead of trampling it.
///
/// Concurrent misses on different blocks of one group are joined by [`GroupLatch`]
/// into a single GET, so a multi-block read or a readahead burst that lands inside
/// one group costs one round trip rather than one per block.
///
/// Sibling blocks are inserted into `sink`, which need not be the cache the
/// demanded block ends up in (see `route_block`).
fn make_fetch(
    tiers: &Arc<Tiers>,
    idx: usize,
    choff: u64,
    source: &Arc<dyn BytesSource + Send + Sync>,
    end: u64,
    permit: Option<OwnedSemaphorePermit>,
    sink: Arc<BlockCache>,
) -> impl FnOnce() -> FetchFuture {
    let tiers = tiers.clone();
    let source = source.clone();
    move || {
        let chlen = tiers.chlen as u64;
        // A whole number of blocks, so every block in the group lands on the same
        // boundaries the cache keys use.
        let group = group_size(tiers.fetch_size, chlen);
        let gbeg = (choff / group) * group;
        let gend = (gbeg + group).min(end);

        Box::pin(async move {
            let fetch_limit = tiers.fetch_limit.clone();
            // One block per group -- the default, with no fetch coalescing. There
            // are no siblings to fill, so the fetched buffer *is* the demanded
            // block and can be handed over whole rather than copied out of.
            //
            // The group latch is skipped here too. With one block per group the
            // group key *is* the foyer key, and foyer already single-flights per
            // key, so the latch could never dedup anything on this path -- it
            // would only add a lock round trip and two map operations per miss.
            // It still earns its keep for multi-block groups below, and in
            // `dual_hybrid`, where a demand routed to the metadata tier and a
            // prefetch routed to the content tier are separate caches holding the
            // same key.
            if group == chlen {
                let bytes = fetch_range(gbeg, gend, source, fetch_limit, permit)
                    .await
                    .map_err(foyer::Error::io_error)?;
                let want = (gend - gbeg) as usize;
                if bytes.len() < want {
                    return Err(foyer::Error::io_error(short_read_error(
                        idx,
                        choff,
                        chlen as usize,
                        bytes.len() as u64,
                    )));
                }
                let mut block = bytes;
                // A source that returned more than asked for must not widen the
                // block; the slicing path below would have trimmed it too.
                block.truncate(want);
                return Ok(block);
            }

            let (bytes, owner) = tiers
                .groups
                .fetch(idx, gbeg, gend, source, fetch_limit, permit)
                .await;
            let bytes = bytes.map_err(|e| {
                foyer::Error::io_error(std::io::Error::new(e.kind(), e.to_string()))
            })?;

            let mut demanded = None;
            for boff in (gbeg..gend).step_by(chlen as usize) {
                let beg = (boff - gbeg) as usize;
                let end = ((boff + chlen).min(gend) - gbeg) as usize;
                // A short read truncates the group; keep whatever blocks came
                // back whole rather than inventing zero-filled tails.
                if end > bytes.len() {
                    break;
                }
                if boff == choff {
                    demanded = Some(bytes[beg..end].to_vec());
                } else if owner {
                    // Only the caller that issued the GET populates the siblings;
                    // a joiner would insert identical bytes under identical keys.
                    sink.insert((idx, boff), bytes[beg..end].to_vec());
                }
            }

            demanded.ok_or_else(|| {
                foyer::Error::io_error(short_read_error(
                    idx,
                    choff,
                    chlen as usize,
                    bytes.len() as u64,
                ))
            })
        })
    }
}

pub(crate) fn short_read_error(idx: usize, off: u64, wanted: usize, got: u64) -> std::io::Error {
    std::io::Error::new(
        std::io::ErrorKind::UnexpectedEof,
        format!("source {idx}: short read at offset {off}: filled {got} of {wanted} bytes"),
    )
}

/// Unwrap a fetch failure back into the `io::Error` it started as, keeping its
/// kind. Flattening to `Error::other` would turn a truncated source's
/// `UnexpectedEof` into `Other` on its way through foyer.
fn foyer_to_io_error(err: foyer::Error) -> std::io::Error {
    let mut kind = std::io::ErrorKind::Other;
    let mut src: Option<&(dyn std::error::Error + 'static)> = Some(&err);
    while let Some(e) = src {
        if let Some(io_err) = e.downcast_ref::<std::io::Error>() {
            kind = io_err.kind();
            break;
        }
        src = e.source();
    }
    std::io::Error::new(kind, err)
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

impl Tiers {
    /// Resolve one block, returning it and whether it had to be fetched.
    ///
    /// Every path looks in the cache *before* building the fetch closure. The
    /// closure owns clones of the source and the tiers, so building it eagerly
    /// would charge every read -- overwhelmingly cache hits -- refcount round
    /// trips on `Arc`s shared by all readers. Building it only on the miss path
    /// costs one extra hash lookup when we do miss, against a fetch that is
    /// about to go to the backing store anyway.
    async fn route_block(
        self: &Arc<Self>,
        idx: usize,
        choff: u64,
        source: &Arc<dyn BytesSource + Send + Sync>,
        end: u64,
        permit: Option<OwnedSemaphorePermit>,
    ) -> Result<(BlockEntry, bool), std::io::Error> {
        let key = (idx, choff);
        let content = &self.content;
        // Siblings from a coalesced fetch go into whichever cache this block is
        // being routed to, so speculation never lands in a tier the read would
        // not have populated itself -- in particular it must not push content
        // blocks into the protected metadata tier during the regular phase.
        if let Some(md) = &self.metadata {
            let md_cache = &md.cache;
            if md.regular_phase.load(Ordering::Acquire) {
                if let Some(entry) = md_cache.get(&key).await.map_err(std::io::Error::other)? {
                    return Ok((entry, false));
                }
                if let Some(entry) = content.get(&key).await.map_err(std::io::Error::other)? {
                    return Ok((entry, false));
                }
                let fetch = make_fetch(self, idx, choff, source, end, permit, content.clone());
                return content
                    .get_or_fetch(&key, fetch)
                    .await
                    .map(|entry| (entry, true))
                    .map_err(foyer_to_io_error);
            }
            // Metadata phase. The demanded block is metadata and goes to the
            // protected tier; the rest of the fetch group is speculation and
            // goes to the content tier, where ordinary eviction can reclaim it.
            // Without this split the metadata tier's footprint is the union of
            // aligned fetch groups, so raising --cache-fetch-size to cut S3
            // round trips inflates the very cache the warming pass exists to
            // produce.
            if let Some(entry) = md_cache.get(&key).await.map_err(std::io::Error::other)? {
                return Ok((entry, false));
            }
            // A sibling that is now demanded is part of the working set after
            // all. Promote it, or the warming pass ends with metadata sitting in
            // an evictable tier and the post-flip reads go back to S3.
            if let Some(entry) = content.get(&key).await.map_err(std::io::Error::other)? {
                let bytes = entry.value().len();
                md_cache.insert(key, entry.value().clone());
                if let Some(log) = &self.io_log {
                    log.log_metadata_insert(idx, choff, bytes, true);
                }
                return Ok((entry, false));
            }
            let fetch = make_fetch(self, idx, choff, source, end, permit, content.clone());
            let entry = md_cache
                .get_or_fetch(&key, fetch)
                .await
                .map_err(foyer_to_io_error)?;
            if let Some(log) = &self.io_log {
                log.log_metadata_insert(idx, choff, entry.value().len(), false);
            }
            return Ok((entry, true));
        }
        if let Some(entry) = content.get(&key).await.map_err(std::io::Error::other)? {
            return Ok((entry, false));
        }
        let fetch = make_fetch(self, idx, choff, source, end, permit, content.clone());
        content
            .get_or_fetch(&key, fetch)
            .await
            .map(|entry| (entry, true))
            .map_err(foyer_to_io_error)
    }

    /// Whether either tier already holds `key`.
    fn contains(&self, key: &(usize, u64)) -> bool {
        self.content.contains(key)
            || self
                .metadata
                .as_ref()
                .is_some_and(|m| m.cache.contains(key))
    }
}

#[async_trait]
impl Cache for FoyerCache {
    async fn read(
        &self,
        idx: usize,
        off: u64,
        buf: &mut [u8],
        trace: &mut ReadTrace,
    ) -> Result<(), std::io::Error> {
        let source = self.sources.get(idx)?;
        let end = source.end();
        let tiers = &self.tiers;
        let chlen = tiers.chlen as u64;

        let csbeg = (off / chlen) * chlen;
        let csend = off + buf.len() as u64;
        let (rabeg, raend) = readahead_block_range(csend, chlen, self.readahead, end);

        // The overwhelming majority of calls (struct-field-sized reads during
        // header/grain-table parsing) need exactly one block. Skip the
        // try_join_all/iterator machinery for that case instead of paying
        // per-call Vec overhead on what's usually a cache hit.
        let mut demand_offs = (csbeg..csend).step_by(tiers.chlen);
        let first = demand_offs.next();
        let second = demand_offs.next();

        let miss = match (first, second) {
            (Some(choff), None) => {
                let (ch, missed) = tiers.route_block(idx, choff, &source, end, None).await?;
                fill_from_block(buf, off, choff, &ch, idx)?;
                trace!("fetched {idx} [{choff},{})", choff + ch.len() as u64);
                missed
            }
            _ => {
                // Fetch each group this read spans exactly once, before routing
                // the individual blocks.
                //
                // `get_or_fetch` commits to fetching the moment it finds a key
                // absent, so fanning every block out at once means all the blocks
                // of a group are committed before the first fetch fills the rest
                // in -- each then pulls the whole group again. The group latch
                // catches that only while the fetches overlap in time; against a
                // fast source they complete one after another and it cannot help.
                // Routing one block per group first collapses them to one fetch,
                // and leaves the rest as cache hits below.
                // Already a whole number of blocks (both constructors normalize
                // it), so this needs no rounding and no division per read.
                let group = tiers.fetch_size as u64;
                if group > chlen {
                    let leaders = (csbeg..csend)
                        .step_by(tiers.chlen)
                        .filter(|choff| {
                            // The first block of the read that falls in this group.
                            let gbeg = (choff / group) * group;
                            *choff == gbeg.max(csbeg)
                        })
                        .map(|choff| tiers.route_block(idx, choff, &source, end, None));
                    // Errors surface again below, where the offending block is
                    // reported precisely; here we only care about warming.
                    let _ = try_join_all(leaders).await;
                }

                let demand = (csbeg..csend)
                    .step_by(tiers.chlen)
                    .map(|choff| tiers.route_block(idx, choff, &source, end, None));
                let chunks = try_join_all(demand).await?;
                let any_missed = chunks.iter().any(|(_, missed)| *missed);

                let mut bbeg = 0u64;
                for (choff, (ch, _)) in (csbeg..csend).step_by(tiers.chlen).zip(chunks) {
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

                any_missed
            }
        };
        trace.foyer_miss = miss;

        let mut prefetched = vec![];

        // One prefetch per fetch group, not per block. The loop spawns without
        // waiting, so `contains` cannot see blocks a still-unfinished group fetch
        // is about to fill; spawning per block would refetch the same group once
        // for every block of it that the window covers.
        let ra_group = tiers.fetch_size as u64;
        let mut last_group: Option<u64> = None;

        for choff in (rabeg..raend).step_by(tiers.chlen) {
            // Don't spawn work for blocks we already hold. Without this, a
            // sequential scan re-spawns a prefetch for every resident block on
            // every read, so task churn tracks read rate rather than miss rate.
            if tiers.contains(&(idx, choff)) {
                continue;
            }

            // A block whose group we have already enqueued this pass arrives with
            // that fetch; asking for it separately would duplicate the GET.
            let gbeg = (choff / ra_group) * ra_group;
            if last_group == Some(gbeg) {
                continue;
            }
            last_group = Some(gbeg);

            // Speculation gets only spare capacity. A prefetch that would have
            // to queue for a permit is dropped instead: it must never make a
            // demand read -- one a client is actually blocked on -- wait behind
            // blocks nobody asked for.
            let Some(permit) = tiers.fetch_limit.try_permit() else {
                break;
            };

            // A spawned task outlives this call and so needs owned handles. This
            // is the miss path by construction, so the clones are paid once per
            // enqueued group rather than once per read.
            let source = source.clone();
            let tiers = tiers.clone();
            tokio::spawn(async move {
                let _ = tiers
                    .route_block(idx, choff, &source, end, Some(permit))
                    .await;
            });

            prefetched.push(choff);
        }

        // Only the blocks we actually enqueued: resident ones and ones dropped
        // for lack of spare capacity are deliberately not counted.
        if let Some(log) = &tiers.io_log {
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
    use futures::FutureExt;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::time::Duration;

    const MIB: u64 = 1024 * 1024;
    const CHUNK: usize = 64 * 1024;
    const CHUNK64: u64 = CHUNK as u64;

    fn md(cache: &FoyerCache) -> &BlockCache {
        &cache.tiers.metadata.as_ref().unwrap().cache
    }

    fn content(cache: &FoyerCache) -> &BlockCache {
        &cache.tiers.content
    }

    async fn read_at(
        cache: &FoyerCache,
        off: u64,
        len: usize,
    ) -> Result<ReadTrace, std::io::Error> {
        let mut buf = vec![0u8; len];
        let mut t = ReadTrace::default();
        cache.read(0, off, &mut buf, &mut t).await?;
        Ok(t)
    }

    /// Records the exact byte range of every source read, and how many there
    /// were. Yields once per read so concurrent readers interleave, which is
    /// what lets the single-flight and parallelism tests observe overlap
    /// without sleeping.
    #[derive(Debug, Default)]
    struct RecordingSource {
        len: u64,
        ranges: Arc<std::sync::Mutex<Vec<(u64, u64)>>>,
        active: Arc<AtomicUsize>,
        max_active: Arc<AtomicUsize>,
    }

    impl RecordingSource {
        fn new(len: u64) -> Self {
            Self {
                len,
                ..Default::default()
            }
        }

        fn reads(&self) -> usize {
            self.ranges.lock().unwrap().len()
        }

        fn ranges(&self) -> Vec<(u64, u64)> {
            self.ranges.lock().unwrap().clone()
        }

        /// A handle that shares the recording with the source the cache owns.
        fn handle(&self) -> Self {
            Self {
                len: self.len,
                ranges: self.ranges.clone(),
                active: self.active.clone(),
                max_active: self.max_active.clone(),
            }
        }
    }

    impl BytesSource for RecordingSource {
        fn read(
            &self,
            beg: u64,
            end: u64,
        ) -> futures::future::BoxFuture<'static, Result<Vec<u8>, std::io::Error>> {
            self.ranges.lock().unwrap().push((beg, end));
            let active = self.active.clone();
            let max_active = self.max_active.clone();
            async move {
                let now = active.fetch_add(1, Ordering::SeqCst) + 1;
                max_active.fetch_max(now, Ordering::SeqCst);
                tokio::task::yield_now().await;
                active.fetch_sub(1, Ordering::SeqCst);
                Ok(vec![0u8; (end - beg) as usize])
            }
            .boxed()
        }

        fn end(&self) -> u64 {
            self.len
        }
    }

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

    /// Let spawned prefetches run to completion.
    ///
    /// The sources here never block, so every task that is going to reach the
    /// source does so within a handful of scheduler turns; waiting for `reads`
    /// to arrive bounds the wait, and the extra turns afterwards let anything
    /// that should *not* fetch prove that it does not.
    async fn settle(src: &RecordingSource, reads: usize) {
        let wait = async {
            while src.reads() < reads {
                tokio::task::yield_now().await;
            }
        };
        tokio::time::timeout(Duration::from_secs(5), wait)
            .await
            .unwrap_or_else(|_| panic!("expected {reads} source reads, saw {}", src.reads()));
        for _ in 0..64 {
            tokio::task::yield_now().await;
        }
    }

    /// A single-memory cache over a recording source.
    async fn single(
        fetch: usize,
        readahead: usize,
        permits: usize,
    ) -> (FoyerCache, RecordingSource) {
        let src = RecordingSource::new(64 * MIB);
        let cache = FoyerCache::single_memory(CHUNK, fetch, 64, readahead, permits, None)
            .await
            .unwrap();
        cache.add_source(0, Box::new(src.handle()));
        (cache, src)
    }

    /// A dual-hybrid cache in the metadata phase over a recording source.
    async fn dual(fetch: usize) -> (FoyerCache, RecordingSource, Arc<AtomicBool>) {
        let src = RecordingSource::new(64 * MIB);
        let regular = Arc::new(AtomicBool::new(false));
        let cache =
            FoyerCache::dual_hybrid(CHUNK, fetch, 64, 0, 64, 0, 0, 4, regular.clone(), None)
                .await
                .unwrap();
        cache.add_source(0, Box::new(src.handle()));
        (cache, src, regular)
    }

    /// The block size is the unit of *fetch*: a 4 KiB read must pull a whole
    /// block from the backing store, whatever that block is set to. Against a
    /// high-latency store this is the knob that decides the runtime, since a
    /// range GET costs mostly fixed latency -- so it has to actually reach the
    /// source, not just size the cache.
    #[tokio::test]
    async fn block_size_sets_the_range_fetched_from_the_source() {
        for block in [64 * 1024usize, 1024 * 1024, 8 * 1024 * 1024] {
            let src = RecordingSource::new(64 * MIB);
            let cache = FoyerCache::single_memory(block, block, 64, 0, 8, None)
                .await
                .unwrap();
            cache.add_source(0, Box::new(src.handle()));

            // One 4 KiB read, at an offset inside the second block.
            read_at(&cache, block as u64 + 4096, 4096).await.unwrap();

            assert_eq!(
                src.ranges(),
                vec![(block as u64, 2 * block as u64)],
                "a 4 KiB read must fetch exactly the whole {block}-byte block"
            );
        }
    }

    /// Memory capacity is a byte budget, not an entry count: a 1 GiB budget
    /// must stay 1 GiB whatever the block size.
    #[test]
    fn memory_budget_is_bytes_not_entries() {
        let gib = 1024;
        assert_eq!(mem_entries(gib, 1024 * 1024), 1024);
        assert_eq!(mem_entries(gib, 8 * 1024 * 1024), 128);
        assert_eq!(mem_entries(gib, 16 * 1024 * 1024), 64);

        // Never zero, however small the budget or however large the block: a
        // cache that can hold nothing would miss on every read forever.
        assert_eq!(mem_entries(1, 64 * 1024 * 1024), 1);
        assert_eq!(mem_entries(0, 1024 * 1024), 1);
        assert_eq!(mem_entries(64, 0), 64); // block 0 falls back to 1 MiB
    }

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

    /// Reads during the metadata phase land in the protected tier and stay
    /// there once the phase flips; reads after the flip go to the content tier.
    #[tokio::test]
    async fn metadata_phase_routes_to_metadata_cache_then_protects() {
        let (cache, _src, regular) = dual(CHUNK).await;

        // Metadata phase: block 0.
        read_at(&cache, 0, 4096).await.unwrap();
        assert!(md(&cache).contains(&(0, 0)));
        assert!(!content(&cache).contains(&(0, 0)));

        // Regular phase: block 1.
        regular.store(true, Ordering::Relaxed);
        read_at(&cache, CHUNK64, 4096).await.unwrap();
        assert!(content(&cache).contains(&(0, CHUNK64)));
        assert!(!md(&cache).contains(&(0, CHUNK64)));

        // Block 0 stays where the metadata phase put it.
        read_at(&cache, 0, 4096).await.unwrap();
        assert!(md(&cache).contains(&(0, 0)));
    }

    /// The whole point of a fetch size larger than the block size while the
    /// metadata cache is being built: one GET fills many blocks, but only the
    /// block the filesystem asked for is metadata. The rest are speculation and
    /// belong in the content tier, where ordinary eviction can reclaim them --
    /// otherwise the protected cache's footprint is the union of fetch groups
    /// and raising the fetch size inflates it in proportion. A sibling that is
    /// later demanded is part of the working set after all: it is served from
    /// the content tier without a second GET and promoted, or the warming pass
    /// would not actually have captured it.
    #[tokio::test]
    async fn metadata_phase_splits_a_group_between_tiers_and_promotes_demanded_siblings() {
        let (cache, src, _regular) = dual(4 * CHUNK).await;

        // Demand block 0. One GET covers blocks 0..3.
        read_at(&cache, 0, 4096).await.unwrap();
        assert_eq!(src.reads(), 1, "one fetch for the group");

        assert!(
            md(&cache).contains(&(0, 0)),
            "the demanded block is metadata"
        );
        for sibling in 1..4u64 {
            let key = (0, sibling * CHUNK64);
            assert!(
                !md(&cache).contains(&key),
                "sibling block {sibling} must not enter the metadata tier"
            );
            assert!(
                content(&cache).contains(&key),
                "sibling block {sibling} belongs in the content tier"
            );
        }

        // Demand block 1, a sibling.
        read_at(&cache, CHUNK64, 4096).await.unwrap();
        assert_eq!(
            src.reads(),
            1,
            "the sibling was already in the content tier; no second GET"
        );
        assert!(
            md(&cache).contains(&(0, CHUNK64)),
            "a demanded sibling is promoted into the metadata tier"
        );
    }

    /// The metadata tier's footprint is not the union of fetched ranges, so it
    /// has to be recorded directly or it cannot be measured at all.
    #[tokio::test]
    async fn metadata_inserts_are_traced() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("trace.jsonl");
        let io_log = IoLog::open(&path).unwrap();
        io_log.begin_serving().unwrap();

        let (cache, _src, _regular) = dual(4 * CHUNK).await;
        let cache = cache.with_io_log(Some(io_log.clone()));

        read_at(&cache, 0, 4096).await.unwrap();
        read_at(&cache, CHUNK64, 4096).await.unwrap();

        // Flush by dropping every handle to the log's BufWriter.
        drop(cache);
        drop(io_log);

        let body = std::fs::read_to_string(&path).unwrap();
        let inserts: Vec<&str> = body
            .lines()
            .filter(|l| l.contains(r#""kind":"md_insert""#))
            .collect();
        assert_eq!(inserts.len(), 2, "one demand insert, one promotion: {body}");
        assert!(
            inserts[0].contains(r#""block":0"#) && inserts[0].contains(r#""promoted":false"#),
            "first insert is the demanded block: {}",
            inserts[0]
        );
        assert!(
            inserts[1].contains(&format!(r#""block":{CHUNK64}"#))
                && inserts[1].contains(r#""promoted":true"#),
            "second insert is the promoted sibling: {}",
            inserts[1]
        );
    }

    #[tokio::test]
    async fn dual_hybrid_creates_tempdirs_under_custom_base() {
        let base = tempfile::tempdir().unwrap();
        let before: Vec<_> = std::fs::read_dir(base.path()).unwrap().collect();
        assert_eq!(before.len(), 0);

        let regular = Arc::new(AtomicBool::new(false));
        let _cache = FoyerCache::dual_hybrid(
            64 * 1024,
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
        assert_eq!(
            after.len(),
            2,
            "expected content_dir and metadata_dir under the custom base"
        );
    }

    /// We deliberately do not dedupe single-block fetches ourselves -- foyer's
    /// `get_or_fetch` coalesces concurrent misses for the same key. This pins
    /// that down: if foyer ever stopped single-flighting, we'd silently start
    /// issuing N S3 GETs for one block.
    #[tokio::test]
    async fn concurrent_misses_of_one_block_hit_the_source_once() {
        let (cache, src) = single(CHUNK, 0, 8).await;
        let cache = Arc::new(cache);

        // Eight readers, all wanting different bytes of the *same* block.
        let handles: Vec<_> = (0..8u64)
            .map(|i| {
                let cache = cache.clone();
                tokio::spawn(async move { read_at(&cache, i * 512, 512).await.unwrap() })
            })
            .collect();
        for h in handles {
            h.await.unwrap();
        }

        assert_eq!(
            src.reads(),
            1,
            "concurrent misses of one block must coalesce into a single fetch"
        );
    }

    /// Readahead must still do its job: the blocks after the one we read get
    /// pulled in, so a following sequential read is served without a fetch --
    /// and blocks already resident are not fetched again.
    #[tokio::test]
    async fn readahead_warms_following_blocks() {
        // Plenty of spare capacity (8 permits) for a readahead depth of 2.
        let (cache, src) = single(CHUNK, 2, 8).await;

        read_at(&cache, 0, 512).await.unwrap();
        settle(&src, 3).await;
        assert_eq!(src.reads(), 3, "the demand block plus 2 prefetched blocks");

        // Block 1 was prefetched, so reading it is a cache hit, not a fetch.
        let t = read_at(&cache, CHUNK64, 512).await.unwrap();
        assert!(!t.foyer_miss, "prefetched block must be served from cache");

        // That read slides the window to blocks 2 and 3. Block 2 is already
        // resident and must be skipped, so only block 3 is newly fetched.
        settle(&src, 4).await;
        assert_eq!(
            src.reads(),
            4,
            "only the one block outside the cache should be prefetched"
        );

        // Re-reading block 0 prefetches nothing: every block in its window is
        // already resident. Without suppression this would re-spawn fetches.
        read_at(&cache, 0, 512).await.unwrap();
        settle(&src, 4).await;
        assert_eq!(
            src.reads(),
            4,
            "resident blocks must not be prefetched again"
        );
    }

    /// Speculation only gets spare capacity. With a single permit, the demand
    /// read consumes it and at most one prefetch can claim it afterwards -- the
    /// rest are dropped rather than queued ahead of future demand reads.
    #[tokio::test]
    async fn readahead_takes_only_spare_capacity() {
        // One permit, but a readahead depth of 4.
        let (cache, src) = single(CHUNK, 4, 1).await;

        read_at(&cache, 0, 512).await.unwrap();
        settle(&src, 2).await;

        assert_eq!(
            src.reads(),
            2,
            "the demand block plus at most one prefetch holding the only permit; \
             queued-up prefetches would make this 5"
        );
    }

    /// The trace records the prefetches actually issued -- not the ones skipped
    /// as resident or dropped for lack of spare capacity.
    #[tokio::test]
    async fn prefetch_trace_counts_only_enqueued_blocks() {
        let dir = tempfile::tempdir().unwrap();
        let log_path = dir.path().join("io.jsonl");
        let io_log = IoLog::open(&log_path).unwrap();
        io_log.begin_serving().unwrap();

        let (cache, src) = single(CHUNK, 2, 8).await;
        let cache = cache.with_io_log(Some(io_log.clone()));

        read_at(&cache, 0, 512).await.unwrap();
        settle(&src, 3).await;

        let prefetch_records = || {
            std::fs::read_to_string(&log_path)
                .unwrap()
                .lines()
                .filter(|l| l.contains(r#""kind":"prefetch""#))
                .map(str::to_string)
                .collect::<Vec<_>>()
        };

        let prefetches = prefetch_records();
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
        read_at(&cache, 0, 512).await.unwrap();
        settle(&src, 3).await;
        assert_eq!(
            prefetch_records().len(),
            1,
            "a read that enqueues no prefetches must not emit a record"
        );
    }

    #[tokio::test]
    async fn concurrent_reads_reach_the_source_in_parallel() {
        let (cache, src) = single(CHUNK, 0, 8).await;
        let cache = Arc::new(cache);

        // Four reads of four distinct blocks through the shared handle.
        let handles: Vec<_> = (0..4u64)
            .map(|i| {
                let cache = cache.clone();
                tokio::spawn(async move { read_at(&cache, i * CHUNK64, 4096).await.unwrap() })
            })
            .collect();
        for h in handles {
            h.await.unwrap();
        }

        let max = src.max_active.load(Ordering::SeqCst);
        assert!(
            max >= 2,
            "block fetches never overlapped (max in-flight: {max})"
        );
    }

    /// Fetch coarse, cache fine: with `fetch_size` above the block size, one
    /// miss pulls the whole aligned group in a single source read, and the
    /// sibling blocks it covers become cache hits -- round trips collapse
    /// without coarsening the eviction granularity.
    #[tokio::test]
    async fn coalesced_fetch_fills_sibling_blocks() {
        let (cache, src) = single(4 * CHUNK, 0, 8).await;

        // A miss inside block 5 fetches its whole aligned 4-block group.
        read_at(&cache, 5 * CHUNK64 + 100, 512).await.unwrap();
        assert_eq!(
            src.ranges(),
            vec![(4 * CHUNK64, 8 * CHUNK64)],
            "one aligned group fetch"
        );

        // The three sibling blocks are already resident.
        for blk in [4u64, 6, 7] {
            let t = read_at(&cache, blk * CHUNK64, 512).await.unwrap();
            assert!(!t.foyer_miss, "block {blk} must be served from cache");
        }
        assert_eq!(
            src.reads(),
            1,
            "sibling reads must not go back to the source"
        );
    }

    /// A coalesced fetch never reads past the end of the source: the group is
    /// clamped, and the blocks that do exist are still cached individually.
    #[tokio::test]
    async fn coalesced_fetch_clamps_to_source_end() {
        // Six blocks: the group holding block 5 is [4, 8) blocks, but only
        // blocks 4 and 5 exist.
        let src = RecordingSource::new(6 * CHUNK64);
        let cache = FoyerCache::single_memory(CHUNK, 4 * CHUNK, 16, 0, 8, None)
            .await
            .unwrap();
        cache.add_source(0, Box::new(src.handle()));

        read_at(&cache, 5 * CHUNK64, 512).await.unwrap();
        assert_eq!(
            src.ranges(),
            vec![(4 * CHUNK64, 6 * CHUNK64)],
            "the group must clamp to the source end"
        );

        // Block 4 was in the clamped group and must be resident.
        let t = read_at(&cache, 4 * CHUNK64, 512).await.unwrap();
        assert!(!t.foyer_miss, "the clamped group's sibling must be a hit");
    }

    /// A short read truncates the group: blocks that came back whole are
    /// kept, nothing zero-filled is invented, and a demanded block inside the
    /// truncated region is an error.
    #[tokio::test]
    async fn short_group_read_keeps_whole_blocks_only() {
        let cache = FoyerCache::single_memory(CHUNK, 4 * CHUNK, 16, 0, 4, None)
            .await
            .unwrap();
        // ShortSource returns half of any requested range, so a 4-block group
        // comes back as blocks 0 and 1 only.
        cache.add_source(0, Box::new(ShortSource { len: MIB }));

        read_at(&cache, 0, CHUNK).await.unwrap();

        // Block 1 came back whole and was kept.
        let t = read_at(&cache, CHUNK64, CHUNK).await.unwrap();
        assert!(!t.foyer_miss, "the surviving sibling must be a hit");

        // Block 2 fell inside the truncation; demanding it is an error, not
        // silent zeros.
        let err = read_at(&cache, 2 * CHUNK64, CHUNK).await.unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::UnexpectedEof);
    }

    #[tokio::test]
    async fn short_block_from_source_is_an_error_not_silent_zeros() {
        let cache = FoyerCache::single_memory(CHUNK, CHUNK, 16, 0, 4, None)
            .await
            .unwrap();
        cache.add_source(0, Box::new(ShortSource { len: MIB }));

        // Request exactly one full block; the source returns only half of it.
        let err = read_at(&cache, 0, CHUNK).await.unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::UnexpectedEof);

        // A short block in the middle of a multi-block read must also error
        // rather than underflow the block arithmetic.
        let err = read_at(&cache, 0, 2 * CHUNK).await.unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::UnexpectedEof);
    }

    /// A zero block size is a caller bug, but it must not be a crash: it would
    /// otherwise divide by zero in `group_size` and panic in `step_by`.
    #[tokio::test]
    async fn zero_block_size_falls_back_instead_of_panicking() {
        let cache = FoyerCache::single_memory(0, 0, 16, 0, 4, None)
            .await
            .unwrap();
        let (_dir, src) = test_source();
        cache.add_source(0, src);

        read_at(&cache, 0, 4096).await.unwrap();
    }

    /// Without fetch coalescing the fetched buffer *is* the block, so it must be
    /// moved into the cache, not copied. Proven by identity: the cached entry has
    /// to be the very allocation the source handed back. A copy here is a whole
    /// extra block memcpy on every miss.
    #[tokio::test]
    async fn single_block_group_moves_the_fetched_buffer_instead_of_copying() {
        /// Records the address of the buffer it returns.
        #[derive(Debug)]
        struct PtrSource(Arc<std::sync::Mutex<Vec<usize>>>);

        impl BytesSource for PtrSource {
            fn read(
                &self,
                beg: u64,
                end: u64,
            ) -> futures::future::BoxFuture<'static, Result<Vec<u8>, std::io::Error>> {
                let seen = self.0.clone();
                async move {
                    let v = vec![7u8; (end - beg) as usize];
                    seen.lock().unwrap().push(v.as_ptr() as usize);
                    Ok(v)
                }
                .boxed()
            }
            fn end(&self) -> u64 {
                64 * MIB
            }
        }

        let seen = Arc::new(std::sync::Mutex::new(Vec::new()));
        // fetch == block: no coalescing, the default.
        let cache = FoyerCache::single_memory(CHUNK, CHUNK, 64, 0, 8, None)
            .await
            .unwrap();
        cache.add_source(0, Box::new(PtrSource(seen.clone())));

        read_at(&cache, 0, 512).await.unwrap();

        let fetched_ptr = seen.lock().unwrap()[0];
        let entry = content(&cache).get(&(0, 0)).await.unwrap().unwrap();
        assert_eq!(
            entry.value().as_ptr() as usize,
            fetched_ptr,
            "the fetched buffer must be moved into the cache, not copied out of"
        );
    }

    /// A panicking source read must not outlive the read that triggered it: it
    /// surfaces as an error, and the group's latch entry is retired so a later
    /// miss anywhere in the group retries instead of joining the dead fetch.
    /// Recovery must match a source `Err`.
    #[tokio::test]
    async fn a_panicking_fetch_does_not_poison_the_group() {
        /// Panics on its first read, succeeds afterwards.
        #[derive(Debug)]
        struct PanicOnce(Arc<AtomicUsize>);

        impl BytesSource for PanicOnce {
            fn read(
                &self,
                beg: u64,
                end: u64,
            ) -> futures::future::BoxFuture<'static, Result<Vec<u8>, std::io::Error>> {
                let calls = self.0.clone();
                async move {
                    if calls.fetch_add(1, Ordering::SeqCst) == 0 {
                        panic!("source blew up");
                    }
                    Ok(vec![0u8; (end - beg) as usize])
                }
                .boxed()
            }
            fn end(&self) -> u64 {
                64 * MIB
            }
        }

        let calls = Arc::new(AtomicUsize::new(0));
        let cache = FoyerCache::single_memory(CHUNK, 4 * CHUNK, 64, 0, 8, None)
            .await
            .unwrap();
        cache.add_source(0, Box::new(PanicOnce(calls.clone())));

        // Block 0's fetch panics: the read fails, but as an error, not an unwind.
        assert!(
            read_at(&cache, 0, 512).await.is_err(),
            "a panicking source read must surface as a failed read"
        );

        // A different block of the same group: it must reach the source again
        // rather than join the dead fetch.
        read_at(&cache, CHUNK64, 512)
            .await
            .expect("the group must retry after a panicking fetch, not stay poisoned");
        assert_eq!(
            calls.load(Ordering::SeqCst),
            2,
            "the retry has to actually reach the source"
        );
    }

    /// A fetch size is a byte count a user typed, not necessarily a whole number
    /// of blocks. It has to be rounded down to one: `gbeg` derives from it, and if
    /// a group starts mid-block then no block boundary inside it coincides with a
    /// cache key, so the demanded block is never produced.
    #[test]
    fn fetch_size_is_rounded_down_to_whole_blocks() {
        assert_eq!(group_size(4 * CHUNK, CHUNK64), 4 * CHUNK64);
        // 2.5 blocks -> 2 blocks.
        assert_eq!(group_size(CHUNK * 5 / 2, CHUNK64), 2 * CHUNK64);
        // Never below one block, however small the request.
        assert_eq!(group_size(1, CHUNK64), CHUNK64);
        assert_eq!(group_size(0, CHUNK64), CHUNK64);
    }

    /// The end-to-end consequence of the rounding above: every block stays
    /// readable with an unaligned fetch size, and every GET sits on the block
    /// grid so the blocks it caches are reachable.
    #[tokio::test]
    async fn unaligned_fetch_size_still_reads_every_block() {
        // 2.5 blocks: a legal --cache-fetch-size that is not a multiple.
        let (cache, src) = single(CHUNK * 5 / 2, 0, 8).await;

        for blk in 0..8u64 {
            read_at(&cache, blk * CHUNK64, 512)
                .await
                .unwrap_or_else(|e| panic!("block {blk} must be readable, got {e}"));
        }

        for (beg, end) in src.ranges() {
            assert_eq!(beg % CHUNK64, 0, "group start {beg} is not block-aligned");
            assert_eq!(
                (end - beg) % CHUNK64,
                0,
                "group [{beg},{end}) is not a whole number of blocks"
            );
        }
    }

    /// One read spanning several blocks of a single fetch group is one GET.
    /// `try_join_all` fans the blocks out concurrently and foyer single-flights
    /// per key, not per group, so this is the group latch's job.
    #[tokio::test]
    async fn multi_block_demand_read_fetches_its_group_once() {
        const FETCH: usize = 8 * CHUNK;
        // readahead 0: nothing speculative is involved in this path.
        let (cache, src) = single(FETCH, 0, 16).await;

        read_at(&cache, 0, 4 * CHUNK).await.unwrap();

        assert_eq!(
            src.ranges(),
            vec![(0, FETCH as u64)],
            "a 4-block read inside one group must cost exactly one GET"
        );
    }

    /// The readahead loop spawns a task per block without waiting for the
    /// previous one to land, so `contains` cannot suppress blocks that a
    /// still-in-flight group fetch is about to fill. The group latch has to,
    /// or a readahead window wider than one group refetches it per block.
    #[tokio::test]
    async fn readahead_does_not_refetch_a_group_per_block() {
        // Readahead of 8 blocks over 4-block groups: the window reaches past the
        // demand block's own group into two more.
        let (cache, src) = single(4 * CHUNK, 8, 16).await;

        read_at(&cache, 0, 512).await.unwrap();
        settle(&src, 3).await;

        let got = src.ranges();
        let mut distinct = got.clone();
        distinct.sort();
        distinct.dedup();
        assert_eq!(
            got.len(),
            distinct.len(),
            "each group must be fetched once, got {got:?}"
        );
        assert_eq!(
            distinct,
            vec![
                (0, 4 * CHUNK64),
                (4 * CHUNK64, 8 * CHUNK64),
                (8 * CHUNK64, 12 * CHUNK64),
            ],
            "demand block's group plus the two the readahead window reaches"
        );
    }

    /// Joining an in-flight group must not weaken the short-read contract: a
    /// joiner whose block fell inside the truncated tail still errors rather
    /// than being handed someone else's bytes.
    #[tokio::test]
    async fn group_joiners_still_reject_short_reads() {
        let cache = Arc::new(
            FoyerCache::single_memory(CHUNK, 4 * CHUNK, 64, 0, 8, None)
                .await
                .unwrap(),
        );
        // Returns half of any requested range: a 4-block group yields blocks 0-1.
        cache.add_source(0, Box::new(ShortSource { len: MIB }));

        // Blocks 0 and 2 race on the same group; 0 survives, 2 is in the tail.
        let (a, b) = tokio::join!(read_at(&cache, 0, 512), read_at(&cache, 2 * CHUNK64, 512));
        a.unwrap();
        assert_eq!(
            b.unwrap_err().kind(),
            std::io::ErrorKind::UnexpectedEof,
            "a block inside the truncated tail must error, joined fetch or not"
        );
    }
}
