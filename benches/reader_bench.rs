use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion, Throughput};
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

criterion_group!(name = benches; config = Criterion::default(); targets = full_sequential_read);
criterion_main!(benches);
