//! Serve a VMDK image over NBD (fixed new-style), similar to `qemu-nbd`.

use clap::Parser;
use diskimage_nbd::{
    CommonArgs, NbdImage, init_tracing, make_cache_phase, run_serve, server::open_io_log,
};
use std::{io, process::ExitCode};
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

fn open_reader(
    path: &str,
    readahead: usize,
    s3_concurrency: usize,
    cache_mem_mib: usize,
    cache_mode: CacheMode,
) -> Result<VmdkAdapter, Box<dyn std::error::Error>> {
    VmdkReader::open_with_options(
        path,
        &VmdkReaderOptions {
            foyer_readahead: readahead,
            s3_concurrency,
            cache_mem_mib,
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

fn run(args: Args) -> Result<(), Box<dyn std::error::Error>> {
    let Args { vmdk_path, common } = args;
    let io_log = open_io_log(common.io_log.as_deref())?;
    let cache_mode = if common.metadata_cache {
        CacheMode::DualHybrid {
            content_disk_mib: common.content_cache_disk_mib,
            metadata_mem_mib: common.metadata_cache_mem_mib,
            metadata_disk_mib: common.metadata_cache_disk_mib,
            regular_phase: make_cache_phase(&common)?,
        }
    } else {
        CacheMode::SingleMemory
    };
    let (readahead, s3_concurrency, cache_mem_mib) = (
        common.readahead,
        common.s3_concurrency,
        common.cache_mem_mib,
    );
    let path = vmdk_path.clone();
    run_serve(
        common,
        &vmdk_path,
        move || open_reader(&path, readahead, s3_concurrency, cache_mem_mib, cache_mode),
        io_log,
    )
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
