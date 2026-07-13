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

use std::sync::{Arc, Barrier, Mutex};
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

    /// Concurrent client threads.
    #[arg(long, default_value_t = 1)]
    threads: usize,

    /// Give each thread its own E01Reader instead of sharing one behind a Mutex.
    ///
    /// This is the control for measuring what `read_at_offset(&mut self)` costs.
    /// A server cannot do this today -- one image means one reader means one
    /// lock, so every client serialises. Per-thread readers approximate what a
    /// non-serialising API would allow. They do not share a block cache, so each
    /// thread warms its own; that is the price of the comparison.
    #[arg(long, default_value_t = false, action = clap::ArgAction::Set)]
    reader_per_thread: bool,

    /// Confine random offsets to the first N bytes of the image.
    ///
    /// Uniform random reads over a whole 28 GiB image touch 922k distinct 32 KiB
    /// chunks and essentially never re-read one, so they cannot show whether a
    /// cache of *decompressed* chunks is worth anything. Real clients have
    /// locality -- a filesystem re-reads metadata and issues several 4 KiB reads
    /// inside one chunk. A working set smaller than the image models that: with
    /// 20k reads over 256 MiB (8k chunks) each chunk is read ~2.4 times.
    ///
    /// 0 = the whole image.
    #[arg(long, default_value_t = 0)]
    working_set: u64,

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

    /// Keep an LRU of decompressed chunks in front of the block cache. Only pays
    /// if chunks are re-read; a sequential scan never re-reads one.
    #[arg(long, default_value_t = true, action = clap::ArgAction::Set)]
    decoded_chunk_cache: bool,

}

fn offsets(args: &Args, image_size: u64) -> Vec<u64> {
    let span = match args.working_set {
        0 => image_size,
        n => n.min(image_size),
    };
    let last = span.saturating_sub(args.size as u64);

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

fn reader_options(args: &Args) -> E01ReaderOptions {
    E01ReaderOptions {
        parallel_chunk_reads: args.parallel_chunks,
        parallel_chunk_threads: args.parallel_threads,
        decoded_chunk_cache: args.decoded_chunk_cache,
        ..Default::default()
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

    let reader = E01Reader::open_glob(&args.image, &reader_options(&args))?;

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
    if !args.sequential {
        let span = match args.working_set {
            0 => image_size,
            n => n.min(image_size),
        };
        let chunks = span.div_ceil(chunk_size as u64);
        println!(
            "working set: {} ({} chunks; {:.1} reads per chunk)",
            ByteSize::b(span).display().iec(),
            chunks,
            args.reads as f64 / chunks as f64,
        );
    }
    println!(
        "            {}",
        if args.sequential {
            "sequential"
        } else {
            "random offsets"
        }
    );

    println!(
        "readers    : {}",
        if args.reader_per_thread {
            "one per thread (no shared lock)"
        } else {
            "one, shared behind a Mutex (what a server must do today)"
        }
    );

    let offsets = Arc::new(offsets);
    let shared = (!args.reader_per_thread).then(|| Arc::new(Mutex::new(reader)));

    // Every thread warms up, then waits here, so the timed window starts with
    // all threads ready and all caches in the same state.
    let gate = Arc::new(Barrier::new(args.threads));
    let timer = Arc::new(Mutex::new(None::<Instant>));

    let mut handles = vec![];
    for t in 0..args.threads {
        let offsets = offsets.clone();
        let shared = shared.clone();
        let gate = gate.clone();
        let timer = timer.clone();
        let size = args.size;
        let threads = args.threads;
        let warmup = args.warmup;
        let image = args.image.clone();
        let options = reader_options(&args);

        handles.push(std::thread::spawn(move || {
            // Each thread takes every Nth offset, so together they issue the
            // offset list exactly once.
            let mine: Vec<u64> = offsets.iter().skip(t).step_by(threads).copied().collect();
            let mut buf = vec![0u8; size];

            let mut own = shared
                .is_none()
                .then(|| E01Reader::open_glob(&image, &options).expect("open failed"));

            let mut read = |offset: u64, buf: &mut [u8]| -> usize {
                match (&shared, &mut own) {
                    (Some(shared), _) => shared
                        .lock()
                        .expect("reader lock poisoned")
                        .read_at_offset(offset, buf)
                        .expect("read failed"),
                    (None, Some(own)) => own.read_at_offset(offset, buf).expect("read failed"),
                    _ => unreachable!("a thread has either a shared reader or its own"),
                }
            };

            if warmup {
                for &offset in &mine {
                    read(offset, &mut buf);
                }
            }

            gate.wait();
            if t == 0 {
                *timer.lock().expect("timer poisoned") = Some(Instant::now());
            }
            gate.wait();

            let mut latencies = Vec::with_capacity(mine.len());
            for &offset in &mine {
                let read_start = Instant::now();
                let n = read(offset, &mut buf);
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

    let start = timer
        .lock()
        .expect("timer poisoned")
        .expect("the gate must have started the timer");

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
