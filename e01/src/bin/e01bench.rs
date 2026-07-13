//! Read-pattern harness for a *real* image.
//!
//! `e01verify` only ever reads sequentially, and the criterion benches run
//! against a 1.3 MB toy where decompression -- the actual work -- is a rounding
//! error. Neither can tell you anything about the workload an NBD server
//! actually serves: small, scattered, concurrent reads.
//!
//! This drives that pattern against a real image and reports latency, not just
//! throughput, because latency is what an NBD client waits on.
//!
//! Note `--threads > 1` shares one reader behind a Mutex. That is not a
//! limitation of the harness: `read_at_offset` takes `&mut self`, so a server
//! serving concurrent reads from one image has no other option today.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use bytesize::ByteSize;
use clap::Parser;
use e01::e01_reader::{E01Reader, E01ReaderOptions};
use rand::{Rng, SeedableRng, rngs::StdRng};

#[derive(Parser)]
#[command(about = "Measure e01 read patterns against a real image")]
struct Args {
    /// Path to the image (first segment).
    image: String,

    /// Number of reads to issue.
    #[arg(long, default_value_t = 100_000)]
    reads: usize,

    /// Size of each read, in bytes.
    #[arg(long, default_value_t = 4096)]
    size: usize,

    /// Read sequentially from the start instead of at random offsets.
    #[arg(long)]
    sequential: bool,

    /// Concurrent readers, sharing one reader behind a Mutex (see module docs).
    #[arg(long, default_value_t = 1)]
    threads: usize,

    /// Seed for the random offsets, so runs are comparable.
    #[arg(long, default_value_t = 42)]
    seed: u64,

    /// Read the whole offset list once, untimed, before measuring.
    ///
    /// Without this the benchmark measures the page cache, not the code: every
    /// run uses the same seed and so touches the same ~1 MiB blocks, meaning the
    /// first run pays for the disk and every later one runs warm. Whichever
    /// config you happen to run second then "wins". Use --warmup false only if
    /// you have dropped the caches and *want* to measure cold reads.
    #[arg(long, default_value_t = true, action = clap::ArgAction::Set)]
    warmup: bool,

    /// Decompress a read's chunks in parallel over rayon.
    #[arg(long, default_value_t = true, action = clap::ArgAction::Set)]
    parallel_chunks: bool,

    /// Threads for chunk decompression. 0 = rayon's global pool (one per core).
    #[arg(long, default_value_t = e01::e01_reader::DEFAULT_PARALLEL_CHUNK_THREADS)]
    parallel_threads: usize,

}

fn offsets(args: &Args, image_size: u64) -> Vec<u64> {
    let last = image_size.saturating_sub(args.size as u64);

    if args.sequential {
        (0..args.reads as u64)
            .map(|i| (i * args.size as u64) % last.max(1))
            .collect()
    } else {
        let mut rng = StdRng::seed_from_u64(args.seed);
        (0..args.reads)
            .map(|_| rng.random_range(0..=last))
            .collect()
    }
}

fn percentile(sorted: &[Duration], p: f64) -> Duration {
    if sorted.is_empty() {
        return Duration::ZERO;
    }
    let i = ((sorted.len() - 1) as f64 * p).round() as usize;
    sorted[i]
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = Args::parse();

    let reader = E01Reader::open_glob(
        &args.image,
        &E01ReaderOptions {
            parallel_chunk_reads: args.parallel_chunks,
            parallel_chunk_threads: args.parallel_threads,
            ..Default::default()
        },
    )?;

    let image_size = reader.image_size;
    let chunk_size = reader.chunk_size;
    let offsets = offsets(&args, image_size);

    println!(
        "image      : {} ({} chunks of {})",
        ByteSize::b(image_size).display().iec(),
        reader.chunk_count,
        ByteSize::b(chunk_size as u64).display().iec(),
    );
    let chunks_per_read = args.size.div_ceil(chunk_size) + 1;
    println!(
        "pattern    : {} x {} reads ({}~{} chunks each), {} threads",
        args.reads,
        ByteSize::b(args.size as u64).display().iec(),
        args.size.div_ceil(chunk_size),
        chunks_per_read,
        args.threads,
    );
    println!(
        "fan-out    : {}",
        if args.parallel_chunks { "on" } else { "off" }
    );
    println!(
        "            {}",
        if args.sequential {
            "sequential"
        } else {
            "random offsets"
        }
    );

    let mut reader = reader;

    if args.warmup {
        let warm_start = Instant::now();
        let mut buf = vec![0u8; args.size];
        for &offset in &offsets {
            reader.read_at_offset(offset, &mut buf).expect("read failed");
        }
        println!(
            "warmup     : {:.2}s (untimed; page cache is now in the same state for every config)",
            warm_start.elapsed().as_secs_f64()
        );
    }

    let reader = Arc::new(Mutex::new(reader));
    let offsets = Arc::new(offsets);

    let start = Instant::now();

    let mut handles = vec![];
    for t in 0..args.threads {
        let reader = reader.clone();
        let offsets = offsets.clone();
        let size = args.size;
        let threads = args.threads;

        handles.push(std::thread::spawn(move || {
            let mut buf = vec![0u8; size];
            let mut latencies = Vec::with_capacity(offsets.len() / threads + 1);

            // Each thread takes every Nth offset, so together they issue exactly
            // the offset list once.
            for &offset in offsets.iter().skip(t).step_by(threads) {
                let read_start = Instant::now();
                let n = reader
                    .lock()
                    .expect("reader lock poisoned")
                    .read_at_offset(offset, &mut buf)
                    .expect("read failed");
                latencies.push(read_start.elapsed());
                std::hint::black_box(&buf[..n]);
            }
            latencies
        }));
    }

    let mut latencies: Vec<Duration> = vec![];
    for h in handles {
        latencies.extend(h.join().expect("reader thread panicked"));
    }

    let elapsed = start.elapsed();
    let bytes = (latencies.len() * args.size) as u64;

    latencies.sort_unstable();
    let total: Duration = latencies.iter().sum();
    let mean = total / latencies.len().max(1) as u32;

    println!();
    println!(
        "wall       : {:.2}s",
        elapsed.as_secs_f64()
    );
    println!(
        "throughput : {:.1} MiB/s ({:.0} reads/s)",
        bytes as f64 / (1024.0 * 1024.0) / elapsed.as_secs_f64(),
        latencies.len() as f64 / elapsed.as_secs_f64(),
    );
    println!("latency    : mean {:>9.1?}", mean);
    println!("             p50  {:>9.1?}", percentile(&latencies, 0.50));
    println!("             p99  {:>9.1?}", percentile(&latencies, 0.99));
    println!("             max  {:>9.1?}", latencies.last().copied().unwrap_or_default());
    println!();
    println!("(run under `time` for user/sys -- that is where the block_on churn shows up)");

    Ok(())
}
