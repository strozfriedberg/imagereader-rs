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
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
};

/// At most one active session plus one waiting on the reader lock. The waiter
/// covers a client reconnecting while its old session is still tearing down;
/// anything beyond that would just pile up blocked threads, so those
/// connections are dropped at accept time.
const MAX_PENDING_SESSIONS: usize = 2;

/// RAII session slot: incremented at accept, released when the session thread
/// finishes (or the slot is dropped for any other reason).
struct SessionSlot(Arc<AtomicUsize>);

impl SessionSlot {
    fn try_acquire(counter: &Arc<AtomicUsize>) -> Option<Self> {
        let mut current = counter.load(Ordering::Relaxed);
        loop {
            if current >= MAX_PENDING_SESSIONS {
                return None;
            }
            match counter.compare_exchange_weak(
                current,
                current + 1,
                Ordering::AcqRel,
                Ordering::Relaxed,
            ) {
                Ok(_) => return Some(Self(Arc::clone(counter))),
                Err(observed) => current = observed,
            }
        }
    }
}

impl Drop for SessionSlot {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::AcqRel);
    }
}

/// CLI flags common to every image format; flattened into the binary's `Args`.
#[derive(Parser)]
pub struct CommonArgs {
    /// TCP address to listen on (default NBD port 10809).
    #[arg(long, default_value = "127.0.0.1:10809", conflicts_with = "unix")]
    pub listen: SocketAddr,

    /// Unix domain socket path (recommended for local nbd-client).
    #[cfg(unix)]
    #[arg(long, conflicts_with = "listen")]
    pub unix: Option<PathBuf>,

    /// Prefetch this many foyer blocks (--cache-chunk-size each) ahead on
    /// sequential reads.
    #[arg(long, default_value = "0")]
    pub readahead: usize,

    /// Append JSONL trace of NBD reads for workload analysis.
    #[arg(long)]
    pub io_log: Option<PathBuf>,

    /// Max concurrent in-flight S3 segment byte-range fetches. 0 = serial.
    #[arg(long, default_value = "8")]
    pub s3_concurrency: usize,

    /// Foyer in-memory cache capacity in MiB (a byte budget divided by
    /// --cache-chunk-size to get foyer's entry count).
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

    /// Base directory for foyer's on-disk cache (created as a random subdir
    /// under this path). Defaults to the OS temp directory if unset.
    #[arg(long)]
    pub cache_dir: Option<PathBuf>,

    /// Cache block size in bytes: the granularity blocks are stored and
    /// evicted at (all formats).
    ///
    /// Small blocks let the cache keep exactly what is hot -- a scattered
    /// 200 KB index read should not pin megabytes of junk in a cache under
    /// pressure. This is not the knob that sets S3 round trips; that is
    /// --cache-fetch-size. Memory capacity is a byte budget divided by this,
    /// so it does not change the cache's footprint.
    #[arg(long, default_value = "1048576")]
    pub cache_chunk_size: usize,

    /// Bytes pulled from the backing store per cache miss (all formats).
    /// Defaults to --cache-chunk-size.
    ///
    /// Against a high-latency store this is the most important knob here. A
    /// range GET costs almost entirely fixed latency: measured against S3,
    /// 1 MiB took 221 ms and 16 MiB took 153 ms. The bytes are nearly free;
    /// the round trips are not. An NTFS metadata walk that needed 1,192
    /// fetches at 1 MiB needs 447 at 8 MiB, with each one no slower.
    ///
    /// When larger than --cache-chunk-size, one GET fills several cache
    /// blocks: the demanded block plus its aligned siblings, each a separate
    /// cache entry, so eviction stays fine-grained and untouched siblings are
    /// dropped first. Against a local file leave it unset: the bytes are not
    /// free there, and a big fetch is wasted bandwidth on scattered reads.
    #[arg(long)]
    pub cache_fetch_size: Option<usize>,

    /// Append JSONL per-read cache-hit/miss trace (foyer tier, and e01's
    /// secondary decoded-chunk cache where applicable). Separate from
    /// --io-log, which only captures NBD-protocol-level reads.
    #[arg(long)]
    pub cache_trace_log: Option<PathBuf>,
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
pub fn make_cache_phase(common: &CommonArgs) -> io::Result<Arc<AtomicBool>> {
    let flag = Arc::new(AtomicBool::new(false));
    if common.metadata_cache {
        register_sigusr1(flag.clone())?;
    }
    Ok(flag)
}

/// Register a SIGUSR1 handler that sets `flag` to true.  Must be called before any blocking
/// S3 open — the default disposition for SIGUSR1 is terminate.
pub fn register_sigusr1(flag: Arc<AtomicBool>) -> io::Result<()> {
    let mut signals = signal_hook::iterator::Signals::new([signal_hook::consts::SIGUSR1])
        .map_err(|e| io::Error::other(format!("register SIGUSR1 handler: {e}")))?;
    std::thread::spawn(move || {
        for _ in signals.forever() {
            flag.store(true, Ordering::Release);
            tracing::info!("cache: metadata phase ended, switched to regular phase (SIGUSR1)");
        }
    });
    Ok(())
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
            "foyer memory cache: {} MiB budget, {}-byte blocks",
            args.cache_mem_mib,
            args.cache_chunk_size
        );
    }
    if args.readahead > 0 {
        tracing::info!(
            "foyer readahead: prefetch up to {} blocks ({} bytes each) after each read",
            args.readahead,
            args.cache_chunk_size
        );
    }
    if let Some(fetch) = args.cache_fetch_size {
        // A fetch group is a whole number of blocks, so the requested size is
        // rounded down. Report what will actually be fetched, not what was typed.
        let effective = e01::aligned_fetch_size(fetch, args.cache_chunk_size);
        if effective != fetch {
            tracing::warn!(
                "--cache-fetch-size {} is not a multiple of --cache-chunk-size {}; using {}",
                fetch,
                args.cache_chunk_size,
                effective
            );
        }
        if effective > args.cache_chunk_size {
            tracing::info!(
                "fetch coalescing: {} bytes per backing-store GET, cached as {}-byte blocks",
                effective,
                args.cache_chunk_size
            );
        }
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
    // Hold the lock for the entire connection. Format readers are stateful and
    // not thread-safe. The accept loops spawn one thread per client, but only
    // one client is active at a time — all others block here until the current
    // client disconnects (intentional single-client-at-a-time model).
    // Recover from poisoning: a panic in a previous session must not brick the
    // server forever. The reader is read-only, so its state is still usable.
    let mut reader = reader.lock().unwrap_or_else(|poisoned| {
        tracing::warn!("image reader lock poisoned by a panicked session; recovering");
        poisoned.into_inner()
    });
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
    use std::os::unix::fs::FileTypeExt;

    // Only remove a pre-existing path when it is actually a socket. Removing
    // whatever happens to be there would let a typo like `--unix
    // /data/image.raw` delete a real file. symlink_metadata does not follow
    // symlinks, so a symlink is treated as "not a socket" and left alone.
    match std::fs::symlink_metadata(path) {
        Ok(meta) if meta.file_type().is_socket() => std::fs::remove_file(path)?,
        Ok(_) => {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                format!(
                    "{} already exists and is not a socket; refusing to remove it",
                    path.display()
                ),
            ));
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => {}
        Err(e) => return Err(e),
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
    let sessions = Arc::new(AtomicUsize::new(0));
    for stream in listener.incoming() {
        let stream = match stream {
            Ok(s) => s,
            Err(e) => {
                tracing::error!("accept failed: {e}");
                continue;
            }
        };
        let Some(slot) = SessionSlot::try_acquire(&sessions) else {
            tracing::warn!("dropping connection: one session active and one already waiting");
            continue;
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
            let _slot = slot;
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
    let sessions = Arc::new(AtomicUsize::new(0));
    for stream in listener.incoming() {
        let stream = match stream {
            Ok(s) => s,
            Err(e) => {
                tracing::error!("accept failed: {e}");
                continue;
            }
        };
        let Some(slot) = SessionSlot::try_acquire(&sessions) else {
            tracing::warn!("dropping connection: one session active and one already waiting");
            continue;
        };
        tracing::info!("connection on unix:{}", unix_path.display());

        let reader = Arc::clone(&reader);
        let io_log = io_log.clone();
        std::thread::spawn(move || {
            let _slot = slot;
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

#[cfg(test)]
mod tests {
    use super::*;

    /// Long flags that downstream consumers pass on the command line.
    ///
    /// These are public API: changing one is a breaking change to be
    /// coordinated with that consumer, not a refactor. Adding flags is free.
    const STABLE_FLAGS: &[&str] = &[
        "cache-chunk-size",
        "cache-dir",
        "cache-fetch-size",
        "cache-trace-log",
        "content-cache-disk-mib",
        "io-log",
        "metadata-cache",
        "metadata-cache-disk-mib",
        "unix",
    ];

    #[test]
    fn common_args_keeps_its_public_flags() {
        use clap::CommandFactory;

        let command = CommonArgs::command();
        let present: Vec<&str> = command
            .get_arguments()
            .filter_map(|a| a.get_long())
            .collect();
        let missing: Vec<&str> = STABLE_FLAGS
            .iter()
            .copied()
            .filter(|flag| !present.contains(flag))
            .collect();

        assert!(
            missing.is_empty(),
            "diskimage-nbd no longer offers {missing:?}, which downstream consumers pass. \
             Renaming or removing a flag here breaks them; coordinate the change rather \
             than adjusting this list to match."
        );
    }
    use byteorder::{BigEndian, ReadBytesExt, WriteBytesExt};
    #[cfg(unix)]
    use std::os::unix::net::UnixStream;

    struct ZeroImage {
        size: u64,
    }

    impl NbdImage for ZeroImage {
        fn size(&self) -> u64 {
            self.size
        }

        fn read_at_offset(&mut self, _offset: u64, buf: &mut [u8]) -> io::Result<usize> {
            buf.fill(0);
            Ok(buf.len())
        }
    }

    /// With one client active and one waiting, a third connection must be
    /// dropped instead of piling up another blocked thread; once the first
    /// two disconnect, new connections must be accepted again.
    #[test]
    #[cfg(unix)]
    fn accept_loop_bounds_pending_sessions() {
        use std::time::Duration;

        let sock = std::env::temp_dir().join(format!(
            "diskimage-nbd-test-gate-{}.sock",
            std::process::id()
        ));
        let listener = bind_unix(&sock).unwrap();
        let reader = Arc::new(Mutex::new(ZeroImage { size: 4096 }));
        {
            let reader = Arc::clone(&reader);
            let sock = sock.clone();
            std::thread::spawn(move || run_accept_loop_unix(listener, &sock, reader, None));
        }

        // Client 1 is served (greeting arrives); client 2 queues on the reader
        // lock; client 3 must be dropped without a greeting.
        let c1 = UnixStream::connect(&sock).unwrap();
        c1.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
        let mut magic = [0u8; 8];
        (&c1).read_exact(&mut magic).unwrap();
        assert_eq!(&magic, b"NBDMAGIC");

        let c2 = UnixStream::connect(&sock).unwrap();
        let mut c3 = UnixStream::connect(&sock).unwrap();
        c3.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
        let mut byte = [0u8; 1];
        match c3.read(&mut byte) {
            Ok(0) => {} // EOF: connection dropped, as required
            other => panic!("third connection should be dropped, got {other:?}"),
        }

        // Free both slots; a new client must then be served.
        drop(c1);
        drop(c2);
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        loop {
            let c4 = UnixStream::connect(&sock).unwrap();
            c4.set_read_timeout(Some(Duration::from_millis(500)))
                .unwrap();
            let mut magic = [0u8; 8];
            if (&c4).read_exact(&mut magic).is_ok() {
                assert_eq!(&magic, b"NBDMAGIC");
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "slots never freed after sessions ended"
            );
            std::thread::sleep(Duration::from_millis(50));
        }
        std::fs::remove_file(&sock).ok();
    }

    /// A panic in one session thread poisons the reader mutex; later connections
    /// must still be served rather than failing forever.
    #[test]
    #[cfg(unix)]
    fn serve_connection_recovers_from_poisoned_reader_lock() {
        let reader = Arc::new(Mutex::new(ZeroImage { size: 4096 }));

        let poisoner = Arc::clone(&reader);
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _guard = poisoner.lock().unwrap();
            panic!("poison the reader lock");
        }));
        assert!(reader.is_poisoned());

        let (mut client, server_io) = UnixStream::pair().unwrap();
        let server = std::thread::spawn(move || serve_connection(server_io, reader, None));

        // EXPORT_NAME handshake: if the lock poisoning aborts the session, the
        // NBDMAGIC greeting never arrives and these reads fail.
        let mut magic = [0u8; 8];
        client.read_exact(&mut magic).unwrap();
        assert_eq!(&magic, b"NBDMAGIC");
        let mut ihaveopt = [0u8; 8];
        client.read_exact(&mut ihaveopt).unwrap();
        let _hs_flags = client.read_u16::<BigEndian>().unwrap();
        client.write_u32::<BigEndian>(0b11).unwrap(); // FIXED_NEWSTYLE | NO_ZEROES
        client
            .write_u64::<BigEndian>(0x4948_4156_454F_5054)
            .unwrap();
        client.write_u32::<BigEndian>(1).unwrap(); // NBD_OPT_EXPORT_NAME
        client.write_u32::<BigEndian>(0).unwrap();
        assert_eq!(client.read_u64::<BigEndian>().unwrap(), 4096);
        let _flags = client.read_u16::<BigEndian>().unwrap();

        // NBD_CMD_DISC ends the session cleanly.
        client.write_u32::<BigEndian>(0x2560_9513).unwrap();
        client.write_u16::<BigEndian>(0).unwrap();
        client.write_u16::<BigEndian>(2).unwrap();
        client.write_u64::<BigEndian>(0).unwrap();
        client.write_u64::<BigEndian>(0).unwrap();
        client.write_u32::<BigEndian>(0).unwrap();
        client.flush().unwrap();

        server.join().unwrap().unwrap();
    }

    /// A typo pointing --unix at a real file must not delete it: bind_unix
    /// refuses when the path exists and is not a socket.
    #[test]
    #[cfg(unix)]
    fn bind_unix_refuses_to_clobber_a_regular_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("image.raw");
        std::fs::write(&path, b"precious data").unwrap();

        let err = bind_unix(&path).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::AlreadyExists);
        // the file must still be intact
        assert_eq!(std::fs::read(&path).unwrap(), b"precious data");
    }

    /// A stale socket left by a prior run is a socket, so it is removed and
    /// rebinding succeeds.
    #[test]
    #[cfg(unix)]
    fn bind_unix_replaces_a_stale_socket() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nbd.sock");

        let first = bind_unix(&path).unwrap();
        drop(first); // leaves the socket file behind
        assert!(std::fs::symlink_metadata(&path).is_ok());

        // rebinding removes the stale socket and succeeds
        let _second = bind_unix(&path).unwrap();
    }

    /// A fresh path with nothing at it binds cleanly.
    #[test]
    #[cfg(unix)]
    fn bind_unix_binds_a_fresh_path() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("fresh.sock");
        let _listener = bind_unix(&path).unwrap();
        assert!(std::fs::symlink_metadata(&path).is_ok());
    }
}
