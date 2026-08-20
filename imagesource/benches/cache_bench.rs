use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, LazyLock};
use std::time::Duration;

use criterion::{BenchmarkId, Criterion, SamplingMode, criterion_group, criterion_main};
use futures::future::{BoxFuture, FutureExt};
use imagesource::{BytesSource, Cache, FoyerCache, ReadTrace};
use tokio::runtime::Runtime;

static RT: LazyLock<Runtime> = LazyLock::new(|| Runtime::new().unwrap());

const CHUNK_LEN: usize = 1024 * 1024;
const NUM_BLOCKS: u64 = 1_000_000;

/// A block-sized pattern the synthetic source copies out of.
///
/// Deliberately not zeros, and copied rather than zero-allocated. `vec![0u8; n]`
/// gets its pages from `alloc_zeroed`, which at megabyte sizes is a fresh `mmap`:
/// pages the kernel is not obliged to materialize because they are already zero.
/// Every block handed to the cache was therefore backed by the *same* shared zero
/// page, and a "32 MiB" warm read was really re-reading one 4 KiB page. The
/// warm-read benchmark reported 44 GiB/s -- faster than this machine can move
/// memory, which is the tell -- so it was not measuring what it claimed to.
///
/// The damage was to comparisons: any change that introduced a genuine copy
/// (splitting a fetched group into per-block entries did) forced 32 distinct MiB
/// to be materialized and showed up as a ~4x regression, while like-for-like on
/// realistic data it was actually slightly faster. Copying a non-zero pattern
/// makes every block real memory, so the benchmark measures real traffic.
static PATTERN: LazyLock<Vec<u8>> =
    LazyLock::new(|| (0..CHUNK_LEN).map(|i| (i % 251) as u8).collect());

/// `n` bytes of pattern. A copy, so the pages are genuinely faulted in.
fn patterned(n: usize) -> Vec<u8> {
    let mut v = Vec::with_capacity(n);
    while v.len() < n {
        let take = (n - v.len()).min(PATTERN.len());
        v.extend_from_slice(&PATTERN[..take]);
    }
    v
}

struct SyntheticSource {
    len: u64,
    latency: Duration,
}

impl BytesSource for SyntheticSource {
    fn read(&self, beg: u64, end: u64) -> BoxFuture<'static, Result<Vec<u8>, std::io::Error>> {
        let latency = self.latency;
        let n = (end - beg) as usize;
        async move {
            tokio::time::sleep(latency).await;
            Ok(patterned(n))
        }
        .boxed()
    }

    fn end(&self) -> u64 {
        self.len
    }
}

fn single_threaded_throughput(c: &mut Criterion) {
    const NUM_WARM_BLOCKS: u64 = 32;

    let cache = RT.block_on(async {
        let cache = FoyerCache::single_memory(CHUNK_LEN, CHUNK_LEN, 64, 0, 1, None)
            .await
            .unwrap();
        cache.add_source(
            0,
            Box::new(SyntheticSource {
                len: NUM_BLOCKS * CHUNK_LEN as u64,
                latency: Duration::from_millis(1),
            }),
        );
        cache
    });

    // Warm the cache so the measured loop hits memory, not the synthetic source.
    RT.block_on(async {
        let mut trace = ReadTrace::default();
        let mut buf = vec![0u8; CHUNK_LEN];
        for i in 0..NUM_WARM_BLOCKS {
            cache
                .read(0, i * CHUNK_LEN as u64, &mut buf, &mut trace)
                .await
                .unwrap();
        }
    });

    // A group rather than a bare bench_function, so this can opt into flat
    // sampling: each iteration reads 32 MiB out of the warm cache and takes
    // milliseconds, which is what triggered criterion's "unable to complete 100
    // samples in 5.0s" warning under the default linear mode.
    let mut group = c.benchmark_group("imagesource cache warm read");
    group.sampling_mode(SamplingMode::Flat);
    group.measurement_time(Duration::from_secs(10));

    group.bench_function("32x1MiB", |b| {
        let mut buf = vec![0u8; CHUNK_LEN];
        let mut trace = ReadTrace::default();
        b.iter(|| {
            RT.block_on(async {
                for i in 0..NUM_WARM_BLOCKS {
                    cache
                        .read(0, i * CHUNK_LEN as u64, &mut buf, &mut trace)
                        .await
                        .unwrap();
                }
            });
        });
    });
    group.finish();
}

fn concurrent_read_scaling(c: &mut Criterion) {
    let mut group = c.benchmark_group("imagesource cache concurrent scaling");
    group.sampling_mode(SamplingMode::Flat);
    group.sample_size(20);
    group.measurement_time(Duration::from_secs(10));

    for concurrency in [1usize, 4, 8, 16] {
        group.bench_with_input(
            BenchmarkId::from_parameter(concurrency),
            &concurrency,
            |b, &concurrency| {
                let cache = RT.block_on(async {
                    let cache =
                        FoyerCache::single_memory(CHUNK_LEN, CHUNK_LEN, 64, 0, concurrency, None)
                            .await
                            .unwrap();
                    cache.add_source(
                        0,
                        Box::new(SyntheticSource {
                            len: NUM_BLOCKS * CHUNK_LEN as u64,
                            latency: Duration::from_millis(5),
                        }),
                    );
                    Arc::new(cache)
                });
                let next_block = AtomicU64::new(0);

                b.iter_custom(|iters| {
                    RT.block_on(async {
                        let mut total = Duration::ZERO;
                        for _ in 0..iters {
                            let start_block =
                                next_block.fetch_add(concurrency as u64, Ordering::SeqCst);
                            let started = std::time::Instant::now();
                            let handles: Vec<_> = (0..concurrency)
                                .map(|i| {
                                    let cache = cache.clone();
                                    let block = start_block + i as u64;
                                    tokio::spawn(async move {
                                        let mut buf = vec![0u8; CHUNK_LEN];
                                        let mut trace = ReadTrace::default();
                                        cache
                                            .read(0, block * CHUNK_LEN as u64, &mut buf, &mut trace)
                                            .await
                                            .unwrap();
                                    })
                                })
                                .collect();
                            for h in handles {
                                h.await.unwrap();
                            }
                            total += started.elapsed();
                        }
                        total
                    })
                });
            },
        );
    }
    group.finish();
}

criterion_group!(name = benches; config = Criterion::default().noise_threshold(0.05); targets = single_threaded_throughput, concurrent_read_scaling);
criterion_main!(benches);
