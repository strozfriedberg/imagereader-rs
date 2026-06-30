use crate::{
    IoLog, NbdImage,
    nbd_protocol::{handshake, transmission},
};
use clap::Parser;
#[cfg(unix)]
use std::os::unix::net::UnixListener;
use std::{
    io::{self, Read, Write},
    net::{SocketAddr, TcpListener, TcpStream},
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
};

/// CLI flags shared by every diskimage-nbd binary.  Flatten into format-specific `Args`.
#[derive(Parser)]
pub struct CommonArgs {
    /// TCP address to listen on (default NBD port 10809).
    #[arg(long, default_value = "127.0.0.1:10809", conflicts_with = "unix")]
    pub listen: SocketAddr,

    /// Unix domain socket path (recommended for local nbd-client).
    #[cfg(unix)]
    #[arg(long, conflicts_with = "listen")]
    pub unix: Option<PathBuf>,

    /// Prefetch this many 1 MiB foyer blocks ahead on sequential reads.
    #[arg(long, default_value = "0")]
    pub readahead: usize,

    /// Append JSONL trace of NBD reads for workload analysis.
    #[arg(long)]
    pub io_log: Option<PathBuf>,

    /// Max concurrent in-flight S3 segment byte-range fetches. 0 = serial.
    #[arg(long, default_value = "8")]
    pub s3_concurrency: usize,

    /// Foyer in-memory cache capacity in ~1 MiB entries.
    #[arg(long, default_value = "1024")]
    pub cache_mem_mib: usize,

    /// Enable two-tier metadata cache for cache warming workflows. Starts in metadata phase;
    /// send SIGUSR1 to exit metadata phase and switch to regular content cache.
    #[arg(long)]
    pub metadata_cache: bool,

    /// Content-cache on-disk size (MiB), requires --metadata-cache.
    #[arg(long, default_value = "4096")]
    pub content_cache_disk_mib: usize,

    /// Metadata-cache on-disk size (MiB), requires --metadata-cache.
    #[arg(long, default_value = "4096")]
    pub metadata_cache_disk_mib: usize,

    /// Metadata-cache in-memory capacity (~1 MiB entries), requires --metadata-cache.
    #[arg(long, default_value = "256")]
    pub metadata_cache_mem_mib: usize,
}

pub fn open_io_log(path: Option<&Path>) -> io::Result<Option<Arc<IoLog>>> {
    match path {
        Some(p) => {
            let log = IoLog::open(p)?;
            tracing::info!("io trace: {}", p.display());
            Ok(Some(log))
        }
        None => Ok(None),
    }
}

/// Create the `regular_phase` flag and, when `metadata_cache` is enabled, register a SIGUSR1
/// handler that flips it.  Must be called before any blocking S3 open — the default SIGUSR1
/// disposition is terminate.
pub fn make_cache_phase(common: &CommonArgs) -> Arc<AtomicBool> {
    let flag = Arc::new(AtomicBool::new(false));
    if common.metadata_cache {
        register_sigusr1(flag.clone());
    }
    flag
}

/// Register a SIGUSR1 handler that sets `flag` to true.  Must be called before any blocking
/// S3 open — the default disposition for SIGUSR1 is terminate.
pub fn register_sigusr1(flag: Arc<AtomicBool>) {
    let mut signals = signal_hook::iterator::Signals::new([signal_hook::consts::SIGUSR1])
        .expect("register SIGUSR1 handler");
    std::thread::spawn(move || {
        for _ in signals.forever() {
            flag.store(true, Ordering::Relaxed);
            tracing::info!("cache: metadata phase ended, switched to regular phase (SIGUSR1)");
        }
    });
}

pub fn log_cache_opts(args: &CommonArgs) {
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

pub fn tune_tcp(stream: &TcpStream) {
    let _ = stream.set_nodelay(true);
}

pub fn serve_connection<S, I>(
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

/// Bind a Unix socket, removing any stale socket file first.
#[cfg(unix)]
pub fn bind_unix(path: &Path) -> io::Result<UnixListener> {
    // Call remove_file unconditionally rather than checking exists() first:
    // the exists()+remove_file sequence has a TOCTOU window, and remove_file
    // returning NotFound is harmless (there was no stale file to clear).
    if let Err(e) = std::fs::remove_file(path) {
        if e.kind() != io::ErrorKind::NotFound {
            return Err(e);
        }
    }
    if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty()
    {
        std::fs::create_dir_all(parent)?;
    }
    UnixListener::bind(path)
}

pub fn run_accept_loop_tcp<I>(
    listener: TcpListener,
    reader: Arc<Mutex<I>>,
    io_log: Option<Arc<IoLog>>,
) -> io::Result<()>
where
    I: NbdImage + Send + 'static,
{
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
pub fn run_accept_loop_unix<I>(
    listener: UnixListener,
    unix_path: &Path,
    reader: Arc<Mutex<I>>,
    io_log: Option<Arc<IoLog>>,
) -> io::Result<()>
where
    I: NbdImage + Send + 'static,
{
    for stream in listener.incoming() {
        let stream = match stream {
            Ok(s) => s,
            Err(e) => {
                tracing::error!("accept failed: {e}");
                continue;
            }
        };
        tracing::info!("connection on unix:{}", unix_path.display());

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

/// Open a reader via `open`, then bind the appropriate transport and serve clients.
///
/// On Unix, the socket is bound before `open` is called (bind-before-open) so that
/// orchestrators can detect readiness via the socket path immediately.
pub fn run_serve<I, E, F>(
    common: CommonArgs,
    image_path: &str,
    open: F,
    io_log: Option<Arc<IoLog>>,
) -> Result<(), Box<dyn std::error::Error>>
where
    I: NbdImage + Send + 'static,
    E: Into<Box<dyn std::error::Error>>,
    F: FnOnce() -> Result<I, E>,
{
    #[cfg(unix)]
    if let Some(unix_path) = &common.unix {
        let listener = bind_unix(unix_path)?;
        tracing::info!(
            "socket bound at {}; opening {}",
            unix_path.display(),
            image_path
        );
        let adapter = open().map_err(Into::into)?;
        let image_size = adapter.size();
        let reader = Arc::new(Mutex::new(adapter));
        log_cache_opts(&common);
        tracing::info!(
            "listening on unix:{}; image size {} bytes",
            unix_path.display(),
            image_size
        );
        return run_accept_loop_unix(listener, unix_path, reader, io_log).map_err(Into::into);
    }

    tracing::info!("opening {}", image_path);
    let adapter = open().map_err(Into::into)?;
    let image_size = adapter.size();
    let reader = Arc::new(Mutex::new(adapter));
    log_cache_opts(&common);
    let listener = TcpListener::bind(common.listen)?;
    tracing::info!(
        "listening on {}; image size {} bytes",
        common.listen,
        image_size
    );
    run_accept_loop_tcp(listener, reader, io_log).map_err(Into::into)
}
