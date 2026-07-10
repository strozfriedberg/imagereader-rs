//! Standalone timing probe for `VmdkReader::open_with_options` and subsequent reads.
//!
//! Isolates two costs that are otherwise conflated inside a live diskimage-nbd + fls-warm
//! session: (1) the one-time grain-table load done synchronously at open (fully serial,
//! regardless of --s3-concurrency: see extents.rs read_grain_table_sparse/sesparse), and
//! (2) per-read cold-grain fetch+decompress cost once the reader is open.
//!
//! Mirrors diskimage-nbd's CommonArgs defaults so timings are representative of the
//! deployed server (see diskimage-nbd/src/server.rs CommonArgs).

use clap::Parser;
use std::{
    sync::{Arc, atomic::AtomicBool},
    time::Instant,
};
use vmdkrs::{
    init_tracing,
    vmdk_reader::{CacheMode, VmdkReader, VmdkReaderOptions},
};

#[derive(Parser)]
#[command(about = "Time VmdkReader::open and sampled reads against a real (e.g. s3://) image")]
struct Args {
    /// Path or URL to the vmdk descriptor/image (local path or s3:// URL).
    image_path: String,

    /// Max concurrent in-flight S3 byte-range fetches. 0 = serial. Matches diskimage-nbd default.
    #[arg(long, default_value = "8")]
    s3_concurrency: usize,

    /// Foyer in-memory cache capacity in ~1 MiB entries. Matches diskimage-nbd default.
    #[arg(long, default_value = "1024")]
    cache_mem_mib: usize,

    /// Use the dual metadata/content cache (mirrors diskimage-nbd's --metadata-cache).
    #[arg(long)]
    metadata_cache: bool,

    #[arg(long, default_value = "4096")]
    content_cache_disk_mib: usize,

    #[arg(long, default_value = "4096")]
    metadata_cache_disk_mib: usize,

    #[arg(long, default_value = "256")]
    metadata_cache_mem_mib: usize,

    /// Number of sampled reads to issue after open, spread evenly across the image.
    #[arg(long, default_value = "20")]
    sample_reads: usize,

    /// Size in bytes of each sampled read.
    #[arg(long, default_value = "4096")]
    read_size: usize,

    /// If >0, skip the serial sample and instead open this many independent readers
    /// (each with its own cache/fetch pool, same options) and fire one read from each
    /// concurrently, at distinct offsets. Compares wall-clock for the batch against the
    /// sum of individual times, to see whether concurrent requests actually overlap
    /// (network/endpoint has slack) or whether total time still scales linearly with
    /// count (a shared bottleneck downstream of this process: bandwidth cap, per-path
    /// throttling, etc). NB: each thread pays its own ~open() cost too (own reader
    /// instance), which is included in "elapsed" but reported separately from open time.
    #[arg(long, default_value = "0")]
    concurrent_reads: usize,
}

fn main() {
    let args = Args::parse();
    init_tracing();

    let make_cache_mode = || {
        if args.metadata_cache {
            CacheMode::DualHybrid {
                content_disk_mib: args.content_cache_disk_mib,
                metadata_mem_mib: args.metadata_cache_mem_mib,
                metadata_disk_mib: args.metadata_cache_disk_mib,
                regular_phase: Arc::new(AtomicBool::new(false)),
            }
        } else {
            CacheMode::SingleMemory
        }
    };

    if args.concurrent_reads > 0 {
        run_concurrent(&args, make_cache_mode);
        return;
    }

    let opts = VmdkReaderOptions {
        s3_concurrency: args.s3_concurrency,
        cache_mem_mib: args.cache_mem_mib,
        cache_mode: make_cache_mode(),
        ..VmdkReaderOptions::default()
    };

    eprintln!("opening {} ...", args.image_path);
    let open_start = Instant::now();
    let mut reader = match VmdkReader::open_with_options(&args.image_path, &opts) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("open failed: {e}");
            std::process::exit(1);
        }
    };
    let open_elapsed = open_start.elapsed();
    println!(
        "open: {:.3}s (image_size={} bytes)",
        open_elapsed.as_secs_f64(),
        reader.image_size
    );

    if args.sample_reads == 0 || reader.image_size == 0 {
        return;
    }

    let mut buf = vec![0u8; args.read_size];
    let stride = reader.image_size / args.sample_reads as u64;
    let mut total = std::time::Duration::ZERO;

    for i in 0..args.sample_reads {
        let offset =
            (i as u64 * stride).min(reader.image_size.saturating_sub(args.read_size as u64));
        let t = Instant::now();
        match reader.read_at_offset(offset, &mut buf) {
            Ok(_) => {
                let elapsed = t.elapsed();
                total += elapsed;
                println!(
                    "read[{i}] offset={offset} elapsed={:.3}s",
                    elapsed.as_secs_f64()
                );
            }
            Err(e) => {
                println!("read[{i}] offset={offset} FAILED: {e}");
            }
        }
    }

    println!(
        "sampled {} reads, total {:.3}s, avg {:.3}s/read",
        args.sample_reads,
        total.as_secs_f64(),
        total.as_secs_f64() / args.sample_reads as f64
    );
}

fn run_concurrent(args: &Args, make_cache_mode: impl Fn() -> CacheMode) {
    // One throwaway open just to learn image_size for spreading offsets; not timed.
    let probe_opts = VmdkReaderOptions {
        s3_concurrency: args.s3_concurrency,
        cache_mem_mib: args.cache_mem_mib,
        cache_mode: make_cache_mode(),
        ..VmdkReaderOptions::default()
    };
    let image_size = match VmdkReader::open_with_options(&args.image_path, &probe_opts) {
        Ok(r) => r.image_size,
        Err(e) => {
            eprintln!("probe open failed: {e}");
            std::process::exit(1);
        }
    };
    if image_size == 0 {
        eprintln!("image_size is 0, nothing to read");
        return;
    }

    let n = args.concurrent_reads;
    let stride = image_size / n as u64;
    let read_size = args.read_size;

    eprintln!("firing {n} concurrent reader(s) (each opens its own reader+cache), 1 read each ...");
    let batch_start = Instant::now();

    let results: Vec<(usize, u64, Result<std::time::Duration, std::time::Duration>)> =
        std::thread::scope(|scope| {
            let handles: Vec<_> = (0..n)
                .map(|i| {
                    let image_path = args.image_path.clone();
                    let cache_mode = make_cache_mode();
                    let opts = VmdkReaderOptions {
                        s3_concurrency: args.s3_concurrency,
                        cache_mem_mib: args.cache_mem_mib,
                        cache_mode,
                        ..VmdkReaderOptions::default()
                    };
                    let offset =
                        (i as u64 * stride).min(image_size.saturating_sub(read_size as u64));
                    scope.spawn(move || {
                        let t = Instant::now();
                        let mut reader = match VmdkReader::open_with_options(&image_path, &opts) {
                            Ok(r) => r,
                            Err(e) => {
                                eprintln!("thread {i} open failed: {e}");
                                return (i, offset, Err(t.elapsed()));
                            }
                        };
                        let mut buf = vec![0u8; read_size];
                        match reader.read_at_offset(offset, &mut buf) {
                            Ok(_) => (i, offset, Ok(t.elapsed())),
                            Err(e) => {
                                eprintln!("thread {i} read failed: {e}");
                                (i, offset, Err(t.elapsed()))
                            }
                        }
                    })
                })
                .collect();
            handles.into_iter().map(|h| h.join().unwrap()).collect()
        });

    let wall = batch_start.elapsed();

    let mut sum = std::time::Duration::ZERO;
    for (i, offset, res) in &results {
        match res {
            Ok(d) => {
                sum += *d;
                println!(
                    "thread[{i}] offset={offset} open+read elapsed={:.3}s",
                    d.as_secs_f64()
                );
            }
            Err(d) => println!(
                "thread[{i}] offset={offset} FAILED after {:.3}s",
                d.as_secs_f64()
            ),
        }
    }

    println!(
        "concurrent batch: n={n} wall={:.3}s, sum-of-individual={:.3}s (speedup {:.2}x if fully overlapped)",
        wall.as_secs_f64(),
        sum.as_secs_f64(),
        sum.as_secs_f64() / wall.as_secs_f64().max(0.001)
    );
}
