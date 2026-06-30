//! Serve a VMDK image over NBD (fixed new-style), similar to `qemu-nbd`.

use clap::Parser;
use diskimage_nbd::{
    CommonArgs, NbdImage, init_tracing,
    server::{
        bind_unix, log_cache_opts, open_io_log, register_sigusr1, run_accept_loop_tcp,
        run_accept_loop_unix,
    },
};
use std::{
    error::Error,
    io,
    net::TcpListener,
    process::ExitCode,
    sync::{Arc, Mutex, atomic::AtomicBool},
};
use vmdkrs::vmdk_reader::{CacheMode, VmdkReader, VmdkReaderOptions};

#[derive(Parser)]
#[command(author, version, about = "Serve a VMDK image over NBD", long_about = None)]
struct Args {
    /// Path to a VMDK descriptor/image (local path or s3:// URL).
    vmdk_path: String,

    #[command(flatten)]
    common: CommonArgs,
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

fn open_reader(args: &Args, cache_mode: CacheMode) -> Result<VmdkAdapter, Box<dyn Error>> {
    VmdkReader::open_with_options(
        &args.vmdk_path,
        &VmdkReaderOptions {
            foyer_readahead: args.common.readahead,
            s3_concurrency: args.common.s3_concurrency,
            cache_mem_mib: args.common.cache_mem_mib,
            cache_mode,
            // S3/cache traces via vmdk's own IoLog are a separate concern; the
            // --io-log flag here captures only NBD-level reads via diskimage-nbd's IoLog.
            io_log: None,
            ..VmdkReaderOptions::default()
        },
    )
    .map(VmdkAdapter)
    .map_err(Into::into)
}

fn run(args: Args) -> Result<(), Box<dyn Error>> {
    let io_log = open_io_log(args.common.io_log.as_deref())?;

    let cache_mode = if args.common.metadata_cache {
        let regular_phase = Arc::new(AtomicBool::new(false));
        register_sigusr1(regular_phase.clone());
        CacheMode::DualHybrid {
            content_disk_mib: args.common.content_cache_disk_mib,
            metadata_mem_mib: args.common.metadata_cache_mem_mib,
            metadata_disk_mib: args.common.metadata_cache_disk_mib,
            regular_phase,
        }
    } else {
        CacheMode::SingleMemory
    };

    #[cfg(unix)]
    if let Some(unix_path) = &args.common.unix {
        let listener = bind_unix(unix_path)?;
        tracing::info!(
            "socket bound at {}; opening {}",
            unix_path.display(),
            args.vmdk_path
        );
        let adapter = open_reader(&args, cache_mode)?;
        let image_size = adapter.size();
        let reader = Arc::new(Mutex::new(adapter));
        log_cache_opts(&args.common);
        tracing::info!(
            "listening on unix:{}; image size {} bytes",
            unix_path.display(),
            image_size
        );
        return run_accept_loop_unix(listener, unix_path, reader, io_log).map_err(Into::into);
    }

    tracing::info!("opening {}", args.vmdk_path);
    let adapter = open_reader(&args, cache_mode)?;
    let image_size = adapter.size();
    let reader = Arc::new(Mutex::new(adapter));
    log_cache_opts(&args.common);
    let listener = TcpListener::bind(args.common.listen)?;
    tracing::info!(
        "listening on {}; image size {} bytes",
        args.common.listen,
        image_size
    );
    run_accept_loop_tcp(listener, reader, io_log).map_err(Into::into)
}

fn main() -> ExitCode {
    init_tracing();
    match run(Args::parse()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("{e}");
            ExitCode::FAILURE
        }
    }
}
