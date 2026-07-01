//! Serve an E01 (Expert Witness) image over NBD (fixed new-style), similar to `qemu-nbd`.

use clap::Parser;
use diskimage_nbd::{
    CommonArgs, NbdImage, init_tracing, make_cache_phase, run_serve, server::open_io_log,
};
use e01::e01_reader::{
    CacheMode, CorruptChunkPolicy, CorruptSectionPolicy, E01Reader, E01ReaderOptions,
};
use std::{io, process::ExitCode};

#[derive(Parser)]
#[command(author, version, about = "Serve an E01 image over NBD", long_about = None)]
struct Args {
    /// Path to an E01 segment (glob / multi-segment path as supported by E01Reader::open_glob).
    e01_path: String,

    /// Ignore chunk checksums while reading (less safe).
    #[arg(short, long)]
    ignore_checksums: bool,

    #[command(flatten)]
    common: CommonArgs,
}

struct E01Adapter(E01Reader);

impl NbdImage for E01Adapter {
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
    ignore_checksums: bool,
    readahead: usize,
    s3_concurrency: usize,
    cache_mem_mib: usize,
    cache_mode: CacheMode,
) -> Result<E01Adapter, Box<dyn std::error::Error>> {
    E01Reader::open_glob(
        path,
        &E01ReaderOptions {
            corrupt_section_policy: CorruptSectionPolicy::Error,
            corrupt_chunk_policy: if ignore_checksums {
                CorruptChunkPolicy::Zero
            } else {
                CorruptChunkPolicy::Error
            },
            foyer_readahead: readahead,
            s3_concurrency,
            cache_mem_mib,
            cache_mode,
            // S3/cache traces via e01's own IoLog are a separate concern; the
            // --io-log flag here captures only NBD-level reads via diskimage-nbd's IoLog.
            io_log: None,
        },
    )
    .map(E01Adapter)
    .map_err(Into::into)
}

fn run(args: Args) -> Result<(), Box<dyn std::error::Error>> {
    let Args {
        e01_path,
        ignore_checksums,
        common,
    } = args;
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
    let path = e01_path.clone();
    run_serve(
        common,
        &e01_path,
        move || {
            open_reader(
                &path,
                ignore_checksums,
                readahead,
                s3_concurrency,
                cache_mem_mib,
                cache_mode,
            )
        },
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
