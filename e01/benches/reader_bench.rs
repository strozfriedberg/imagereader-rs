use std::time::Duration;

use criterion::{
    BatchSize, BenchmarkId, Criterion, SamplingMode, Throughput, criterion_group, criterion_main,
    measurement::WallTime,
};
use e01::e01_reader::{E01Reader, E01ReaderOptions};

const IMAGE: &str = "data/image.E01";
const BUF_SIZES: [usize; 3] = [4 * 1024, 64 * 1024, 1024 * 1024];

const RANDOM_BUF_SIZE: usize = 4096;
const NUM_OFFSETS: usize = 500;

fn open(options: &E01ReaderOptions) -> E01Reader {
    E01Reader::open_glob(IMAGE, options).unwrap()
}

/// Read the whole image front to back through `buf`.
fn read_all(reader: &E01Reader, buf: &mut [u8]) {
    let mut offset = 0u64;
    loop {
        let n = reader.read_at_offset(offset, buf).unwrap();
        if n == 0 {
            break;
        }
        offset += n as u64;
    }
}

fn random_offsets(image_size: u64) -> Vec<u64> {
    use rand::{Rng, SeedableRng, rngs::StdRng};

    let mut rng = StdRng::seed_from_u64(42);
    (0..NUM_OFFSETS)
        .map(|_| rng.random_range(0..image_size - RANDOM_BUF_SIZE as u64))
        .collect()
}

/// Every iteration here runs in milliseconds, where criterion's default linear
/// sampling would need ~5050 iterations to collect 100 samples. Flat sampling
/// costs 100, and the per-sample timer overhead that linear mode exists to
/// cancel out is negligible at this scale.
fn configure(group: &mut criterion::BenchmarkGroup<'_, WallTime>) {
    group.sampling_mode(SamplingMode::Flat);
    group.measurement_time(Duration::from_secs(10));
}

// ---------------------------------------------------------------------------
// Warm cache: one reader, reused across iterations, so after the first pass
// every chunk is decoded and resident. This measures the steady-state read path
// -- the copy out of the decoded-chunk cache -- which is what a client re-reading
// hot regions of an image hits.
// ---------------------------------------------------------------------------

fn warm_sequential_read(c: &mut Criterion) {
    let options = E01ReaderOptions::default();
    let mut group = c.benchmark_group("e01 sequential read (warm cache)");
    configure(&mut group);

    for buf_size in BUF_SIZES {
        let reader = open(&options);
        group.throughput(Throughput::Bytes(reader.image_size));
        group.bench_with_input(
            BenchmarkId::from_parameter(buf_size),
            &buf_size,
            |b, &buf_size| {
                let mut buf = vec![0u8; buf_size];
                b.iter(|| read_all(&reader, &mut buf));
            },
        );
    }
    group.finish();
}

fn warm_random_read(c: &mut Criterion) {
    let options = E01ReaderOptions::default();
    let reader = open(&options);
    let offsets = random_offsets(reader.image_size);

    let mut group = c.benchmark_group("e01 random read (warm cache)");
    configure(&mut group);
    group.throughput(Throughput::Bytes((RANDOM_BUF_SIZE * offsets.len()) as u64));

    group.bench_function("4KiB_x500", |b| {
        let mut buf = vec![0u8; RANDOM_BUF_SIZE];
        b.iter(|| {
            for &offset in &offsets {
                reader.read_at_offset(offset, &mut buf).unwrap();
            }
        });
    });
    group.finish();
}

// ---------------------------------------------------------------------------
// Cold cache: a fresh reader per iteration, so every chunk has to be fetched
// from the backing store, checksummed and decompressed. Cache population, chunk
// decode and fetch concurrency all live on this path -- the warm benches above
// cannot see a regression in any of them.
//
// The reader is built in `setup`, so segment-table parsing is not timed. The
// backing file is still in the OS page cache, so this measures decode and
// cache-fill cost, not device I/O. For the latency-bound case (S3), see
// imagesource's cache_bench, which injects latency at the BytesSource.
//
// Expect run-to-run variance here, and do not read a cross-session comparison as
// a code change. At a 1 MiB block size a few-MiB image is only a handful of
// misses, and each miss costs several cross-thread handoffs through tokio and
// foyer -- so these benchmarks are dominated by scheduling latency, not by read
// work. Measured on a 2-vCPU VM: one cold 4 KiB read took 30-42 ms where a whole
// warm pass over the same image took 2.4 ms, and consecutive cold passes inside
// one process ranged 122-171 ms. The same benchmarks track bare metal within 5%
// on the warm path. Judge a change by recording the baseline and the comparison
// back-to-back in one session.
// ---------------------------------------------------------------------------

fn cold_sequential_read(c: &mut Criterion) {
    let options = E01ReaderOptions::default();
    let mut group = c.benchmark_group("e01 sequential read (cold cache)");
    configure(&mut group);

    for buf_size in BUF_SIZES {
        group.throughput(Throughput::Bytes(open(&options).image_size));
        group.bench_with_input(
            BenchmarkId::from_parameter(buf_size),
            &buf_size,
            |b, &buf_size| {
                let mut buf = vec![0u8; buf_size];
                b.iter_batched(
                    || open(&options),
                    |reader| {
                        read_all(&reader, &mut buf);
                        reader
                    },
                    BatchSize::PerIteration,
                );
            },
        );
    }
    group.finish();
}

fn cold_random_read(c: &mut Criterion) {
    let options = E01ReaderOptions::default();
    let offsets = random_offsets(open(&options).image_size);

    let mut group = c.benchmark_group("e01 random read (cold cache)");
    configure(&mut group);
    group.throughput(Throughput::Bytes((RANDOM_BUF_SIZE * offsets.len()) as u64));

    group.bench_function("4KiB_x500", |b| {
        let mut buf = vec![0u8; RANDOM_BUF_SIZE];
        b.iter_batched(
            || open(&options),
            |reader| {
                for &offset in &offsets {
                    reader.read_at_offset(offset, &mut buf).unwrap();
                }
                // Return this so that reader teardown isn't part of the bench timing
                reader
            },
            BatchSize::PerIteration,
        );
    });
    group.finish();
}

criterion_group!(
    name = e01_benches;
    config = Criterion::default().noise_threshold(0.05);
    targets = warm_sequential_read, warm_random_read, cold_sequential_read, cold_random_read
);
criterion_main!(e01_benches);
