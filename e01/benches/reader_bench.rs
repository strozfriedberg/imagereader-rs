use criterion::{BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use e01::e01_reader::{E01Reader, E01ReaderOptions};

fn full_sequential_read(c: &mut Criterion) {
    let options = E01ReaderOptions::default();
    let mut group = c.benchmark_group("read_at_offset sequential");
    for buf_size in [4 * 1024usize, 64 * 1024, 1024 * 1024] {
        let mut reader = E01Reader::open_glob("data/image.E01", &options).unwrap();
        let image_size = reader.image_size;
        group.throughput(Throughput::Bytes(image_size));
        group.bench_with_input(
            BenchmarkId::from_parameter(buf_size),
            &buf_size,
            |b, &buf_size| {
                let mut buf = vec![0u8; buf_size];
                b.iter(|| {
                    let mut offset = 0u64;
                    loop {
                        let n = reader.read_at_offset(offset, &mut buf).unwrap();
                        if n == 0 {
                            break;
                        }
                        offset += n as u64;
                    }
                });
            },
        );
    }
    group.finish();
}

fn random_offset_reads(c: &mut Criterion) {
    use rand::{Rng, SeedableRng, rngs::StdRng};

    const BUF_SIZE: usize = 4096;
    const NUM_OFFSETS: usize = 500;

    let options = E01ReaderOptions::default();
    let mut reader = E01Reader::open_glob("data/image.E01", &options).unwrap();
    let image_size = reader.image_size;

    let mut rng = StdRng::seed_from_u64(42);
    let offsets: Vec<u64> = (0..NUM_OFFSETS)
        .map(|_| rng.random_range(0..image_size - BUF_SIZE as u64))
        .collect();

    let mut group = c.benchmark_group("read_at_offset random");
    group.throughput(Throughput::Bytes((BUF_SIZE * offsets.len()) as u64));
    group.bench_function("4KiB_x500", |b| {
        let mut buf = vec![0u8; BUF_SIZE];
        b.iter(|| {
            for &offset in &offsets {
                reader.read_at_offset(offset, &mut buf).unwrap();
            }
        });
    });
    group.finish();
}

criterion_group!(name = e01_benches; config = Criterion::default(); targets = full_sequential_read, random_offset_reads);
criterion_main!(e01_benches);
