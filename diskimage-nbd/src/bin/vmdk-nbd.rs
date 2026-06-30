//! Serve a VMDK image over NBD (fixed new-style), similar to `qemu-nbd`.

use clap::Parser;
use diskimage_nbd::{
    IoLog, NbdImage, init_tracing,
    nbd_protocol::{handshake, transmission},
};
#[cfg(unix)]
use std::os::unix::net::UnixListener;
use std::{
    error::Error,
    io::{self, Read, Write},
    net::{SocketAddr, TcpListener, TcpStream},
    path::PathBuf,
    process::ExitCode,
    sync::{Arc, Mutex, atomic::AtomicBool},
};
use vmdkrs::vmdk_reader::{CacheMode, VmdkReader, VmdkReaderOptions};

#[derive(Parser)]
#[command(author, version, about = "Serve a VMDK image over NBD", long_about = None)]
struct Args {
    /// Path to a VMDK descriptor/image (local path or s3:// URL).
    vmdk_path: String,

    /// TCP address to listen on (default NBD port 10809).
    #[arg(long, default_value = "127.0.0.1:10809", conflicts_with = "unix")]
    listen: SocketAddr,

    /// Unix domain socket path (recommended for local nbd-client).
    #[cfg(unix)]
    #[arg(long, conflicts_with = "listen")]
    unix: Option<PathBuf>,

    /// Prefetch this many 1 MiB foyer blocks ahead on sequential reads (S3/file backing
    /// cache). Useful for linear scans; leave at 0 for random NBD access.
    #[arg(long, default_value = "0")]
    readahead: usize,

    /// Append JSONL trace of NBD reads for workload analysis.
    #[arg(long)]
    io_log: Option<PathBuf>,

    /// Max concurrent in-flight S3 segment byte-range fetches. 0 = serial.
    #[arg(long, default_value = "8")]
    s3_concurrency: usize,

    /// Foyer in-memory cache capacity in ~1 MiB entries.
    #[arg(long, default_value = "1024")]
    cache_mem_mib: usize,

    /// Enable two-tier metadata cache for cache warming workflows. Starts in metadata phase;
    /// send SIGUSR1 to exit metadata phase and switch to regular content cache.
    #[arg(long, default_value = "false")]
    metadata_cache: bool,

    /// Content-cache on-disk size (MiB), requires --metadata-cache.
    #[arg(long, default_value = "4096")]
    content_cache_disk_mib: usize,

    /// Metadata-cache on-disk size (MiB), requires --metadata-cache.
    #[arg(long, default_value = "4096")]
    metadata_cache_disk_mib: usize,

    /// Metadata-cache in-memory capacity (~1 MiB entries), requires --metadata-cache.
    #[arg(long, default_value = "256")]
    metadata_cache_mem_mib: usize,
}

struct VmdkAdapter(VmdkReader);

impl NbdImage for VmdkAdapter {
    fn size(&self) -> u64 {
        self.0.image_size
    }

    fn read_at_offset(&mut self, offset: u64, buf: &mut [u8]) -> io::Result<usize> {
        self.0
            .read_at_offset(offset, buf)
            .map_err(|e| io::Error::other(e.to_string()))
    }
}

fn tune_tcp(stream: &TcpStream) {
    let _ = stream.set_nodelay(true);
}

fn serve_connection<S, I>(
    mut stream: S,
    reader: Arc<Mutex<I>>,
    io_log: Option<Arc<IoLog>>,
) -> io::Result<()>
where
    S: Read + Write,
    I: NbdImage,
{
    let mut reader = reader.lock().unwrap();
    let export_size = reader.size();
    handshake(&mut stream, export_size)?;
    if let Some(log) = &io_log {
        let _ = log.begin_serving();
    }
    let result = transmission(&mut stream, &mut *reader, export_size, io_log.as_ref());
    if let Some(log) = io_log {
        log.log_summary();
    }
    result
}

fn open_reader(args: &Args, cache_mode: CacheMode) -> Result<VmdkAdapter, Box<dyn Error>> {
    Ok(VmdkReader::open_with_options(
        &args.vmdk_path,
        &VmdkReaderOptions {
            foyer_readahead: args.readahead,
            s3_concurrency: args.s3_concurrency,
            cache_mem_mib: args.cache_mem_mib,
            cache_mode,
            // S3/cache traces via vmdk's own IoLog are a separate concern; the
            // --io-log flag here captures only NBD-level reads via diskimage-nbd's IoLog.
            io_log: None,
            ..VmdkReaderOptions::default()
        },
    )
    .map(VmdkAdapter)?)
}

fn log_cache_opts(args: &Args) {
    if args.metadata_cache {
        tracing::info!(
            "content disk cache: {} MiB for S3 segment byte ranges",
            args.content_cache_disk_mib
        );
        tracing::info!(
            "metadata disk cache: {} MiB; memory cache: {} MiB",
            args.metadata_cache_disk_mib,
            args.metadata_cache_mem_mib
        );
        tracing::info!("metadata phase active; send SIGUSR1 to switch to regular phase");
        tracing::info!(
            "s3 fetch concurrency: {} in-flight segment range GETs (0 = serial)",
            args.s3_concurrency
        );
        tracing::info!(
            "foyer memory cache: {} x ~1 MiB entries",
            args.cache_mem_mib
        );
    }

    if args.readahead > 0 {
        tracing::info!(
            "foyer readahead: prefetch up to {} MiB ({} x 1 MiB blocks) after each read",
            args.readahead,
            args.readahead
        );
    }
}

fn run_tcp(
    args: &Args,
    reader: Arc<Mutex<VmdkAdapter>>,
    io_log: Option<Arc<IoLog>>,
) -> Result<(), Box<dyn Error>> {
    let image_size = reader.lock().unwrap().0.image_size;
    let listener = TcpListener::bind(args.listen)?;
    tracing::info!(
        "listening on {}; image size {} bytes",
        args.listen,
        image_size
    );

    for stream in listener.incoming() {
        let stream = match stream {
            Ok(s) => s,
            Err(e) => {
                tracing::error!("accept failed: {e}");
                continue;
            }
        };
        tune_tcp(&stream);
        let peer = stream
            .peer_addr()
            .map(|a| a.to_string())
            .unwrap_or_else(|_| "unknown".to_string());
        tracing::info!("connection from {peer}");

        let reader = Arc::clone(&reader);
        let io_log = io_log.clone();
        std::thread::spawn(move || {
            if let Err(e) = serve_connection(stream, reader, io_log) {
                tracing::warn!("session ended: {e}");
            }
        });
    }

    Ok(())
}

#[cfg(unix)]
fn run_unix(
    args: &Args,
    cache_mode: CacheMode,
    io_log: Option<Arc<IoLog>>,
) -> Result<(), Box<dyn Error>> {
    let path = args.unix.as_ref().expect("unix path required");
    if path.exists() {
        std::fs::remove_file(path)?;
    }
    if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty()
    {
        std::fs::create_dir_all(parent)?;
    }

    // Bind before the (potentially slow) S3 open so orchestrators can see the socket exists.
    let listener = UnixListener::bind(path)?;
    tracing::info!(
        "socket bound at {}; opening {}",
        path.display(),
        args.vmdk_path
    );

    let reader = Arc::new(Mutex::new(open_reader(args, cache_mode)?));
    let image_size = reader.lock().unwrap().0.image_size;
    log_cache_opts(args);
    tracing::info!(
        "listening on unix:{}; image size {} bytes",
        path.display(),
        image_size
    );

    for stream in listener.incoming() {
        let stream = match stream {
            Ok(s) => s,
            Err(e) => {
                tracing::error!("accept failed: {e}");
                continue;
            }
        };
        tracing::info!("connection on unix:{}", path.display());

        let reader = Arc::clone(&reader);
        let io_log = io_log.clone();
        std::thread::spawn(move || {
            if let Err(e) = serve_connection(stream, reader, io_log) {
                tracing::warn!("session ended: {e}");
            }
        });
    }

    Ok(())
}

fn run(args: Args) -> Result<(), Box<dyn Error>> {
    let io_log = match &args.io_log {
        Some(path) => {
            let log = IoLog::open(path)?;
            tracing::info!("io trace: {}", path.display());
            Some(log)
        }
        None => None,
    };

    let regular_phase = Arc::new(AtomicBool::new(false));
    let cache_mode = if args.metadata_cache {
        CacheMode::DualHybrid {
            content_disk_mib: args.content_cache_disk_mib,
            metadata_mem_mib: args.metadata_cache_mem_mib,
            metadata_disk_mib: args.metadata_cache_disk_mib,
            regular_phase: regular_phase.clone(),
        }
    } else {
        CacheMode::SingleMemory
    };
    if args.metadata_cache {
        // Default SIGUSR1 disposition is *terminate*; register before any S3 open.
        let flag = regular_phase.clone();
        let mut signals = signal_hook::iterator::Signals::new([signal_hook::consts::SIGUSR1])
            .expect("register SIGUSR1 handler");
        std::thread::spawn(move || {
            for _ in signals.forever() {
                flag.store(true, std::sync::atomic::Ordering::Relaxed);
                tracing::info!("cache: metadata phase ended, switched to regular phase (SIGUSR1)");
            }
        });
    }

    #[cfg(unix)]
    if args.unix.is_some() {
        return run_unix(&args, cache_mode, io_log);
    }

    tracing::info!("opening {}", args.vmdk_path);
    let reader = Arc::new(Mutex::new(open_reader(&args, cache_mode)?));
    log_cache_opts(&args);
    run_tcp(&args, reader, io_log)
}

fn main() -> ExitCode {
    init_tracing();

    let args = Args::parse();

    match run(args) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("{e}");
            ExitCode::FAILURE
        }
    }
}
