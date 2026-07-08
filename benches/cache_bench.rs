use std::sync::LazyLock;
use std::time::Duration;

use criterion::{criterion_group, criterion_main, Criterion};
use e01::{BytesSource, Cache, FoyerCache, ReadTrace};
use futures::future::{BoxFuture, FutureExt};
use tokio::runtime::Runtime;

static RT: LazyLock<Runtime> = LazyLock::new(|| Runtime::new().unwrap());

const CHUNK_LEN: usize = 1024 * 1024;
const NUM_BLOCKS: u64 = 1_000_000;

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
            Ok(vec![0u8; n])
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
        let cache = FoyerCache::single_memory(CHUNK_LEN, 64, 0, 1, None)
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

    c.bench_function("cache single-threaded warm read", |b| {
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
}

criterion_group!(name = benches; config = Criterion::default(); targets = single_threaded_throughput);
criterion_main!(benches);
