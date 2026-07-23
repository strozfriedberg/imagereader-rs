//! Serve an E01 or VMDK image over NBD (fixed new-style), similar to `qemu-nbd`.
//! The image format (E01 vs VMDK) is chosen by the input path's extension.

use clap::Parser;
use diskimage_nbd::{
    CommonArgs, NbdImage, init_tracing, make_cache_phase, run_serve, server::open_io_log,
};
use e01::IoLog as E01IoLog;
use e01::e01_reader::{
    CacheMode as E01CacheMode, CorruptChunkPolicy, CorruptSectionPolicy, E01Reader,
    E01ReaderOptions,
};
use std::{
    io,
    path::{Path, PathBuf},
    process::ExitCode,
};
use vmdkrs::IoLog as VmdkIoLog;
use vmdkrs::vmdk_reader::{CacheMode as VmdkCacheMode, VmdkReader, VmdkReaderOptions};

#[derive(Parser)]
// long_version (shown by `--version`) adds the build commit; `-V` stays plain.
#[command(
    author,
    version,
    long_version = buildinfo::long_version!(),
    about = "Serve an E01 or VMDK image over NBD",
    long_about = None
)]
struct Args {
    /// Path to an E01 segment or VMDK descriptor/image (local path, glob, or s3:// URL).
    /// Format is chosen by extension: .e01 -> E01, .vmdk -> VMDK.
    image_path: String,

    /// Ignore chunk checksums while reading (E01 only; silently has no effect for VMDK).
    #[arg(short, long)]
    ignore_checksums: bool,

    #[command(flatten)]
    common: CommonArgs,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Format {
    E01,
    Vmdk,
}

fn detect_format(path: &str) -> Result<Format, String> {
    match Path::new(path)
        .extension()
        .and_then(|ext| ext.to_str())
        .map(|ext| ext.to_ascii_lowercase())
        .as_deref()
    {
        Some("e01") => Ok(Format::E01),
        Some("vmdk") => Ok(Format::Vmdk),
        _ => Err(format!(
            "unsupported image extension in {path:?}; expected .e01 or .vmdk"
        )),
    }
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

enum Adapter {
    E01(E01Adapter),
    Vmdk(VmdkAdapter),
}

impl NbdImage for Adapter {
    fn size(&self) -> u64 {
        match self {
            Adapter::E01(a) => a.size(),
            Adapter::Vmdk(a) => a.size(),
        }
    }

    fn read_at_offset(&mut self, offset: u64, buf: &mut [u8]) -> io::Result<usize> {
        match self {
            Adapter::E01(a) => a.read_at_offset(offset, buf),
            Adapter::Vmdk(a) => a.read_at_offset(offset, buf),
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn open_e01(
    path: &str,
    ignore_checksums: bool,
    readahead: usize,
    s3_concurrency: usize,
    cache_mem_mib: usize,
    cache_mode: E01CacheMode,
    cache_dir: Option<PathBuf>,
    cache_block_size: usize,
    cache_fetch_size: usize,
    cache_trace_log: Option<&Path>,
) -> Result<Adapter, Box<dyn std::error::Error>> {
    let io_log = cache_trace_log.map(E01IoLog::open).transpose()?;
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
            cache_block_size,
            cache_fetch_size,
            cache_mode,
            cache_dir,
            // --io-log captures only NBD-level reads via diskimage-nbd's IoLog;
            // --cache-trace-log carries e01's own per-read foyer/decoded-chunk
            // hit/miss trace.
            io_log,
            // Defaults, for now: parallel_chunk_reads/_threads bound the rayon
            // pool that decompresses a read's chunks, and decoded_chunk_cache is
            // the LRU of decompressed chunks. Both are worth revisiting for a
            // server -- see docs/perf-notes.md -- but keep behaviour unchanged here.
            ..Default::default()
        },
    )
    .map(|r| Adapter::E01(E01Adapter(r)))
    .map_err(Into::into)
}

#[allow(clippy::too_many_arguments)]
fn open_vmdk(
    path: &str,
    readahead: usize,
    s3_concurrency: usize,
    cache_mem_mib: usize,
    cache_mode: VmdkCacheMode,
    cache_dir: Option<PathBuf>,
    cache_chunk_size: usize,
    cache_fetch_size: usize,
    cache_trace_log: Option<&Path>,
) -> Result<Adapter, Box<dyn std::error::Error>> {
    let io_log = cache_trace_log.map(VmdkIoLog::open).transpose()?;
    VmdkReader::open_with_options(
        path,
        &VmdkReaderOptions {
            foyer_readahead: readahead,
            s3_concurrency,
            cache_mem_mib,
            cache_mode,
            cache_dir,
            // --io-log captures only NBD-level reads via diskimage-nbd's IoLog;
            // --cache-trace-log carries vmdk's own per-read foyer hit/miss trace.
            io_log,
            cache_chunk_size,
            cache_fetch_size,
        },
    )
    .map(|r| Adapter::Vmdk(VmdkAdapter(r)))
    .map_err(Into::into)
}

fn run(args: Args) -> Result<(), Box<dyn std::error::Error>> {
    let Args {
        image_path,
        ignore_checksums,
        common,
    } = args;
    let format = detect_format(&image_path)?;
    let io_log = open_io_log(common.io_log.as_deref())?;
    let (readahead, s3_concurrency, cache_mem_mib, cache_chunk_size) = (
        common.readahead,
        common.s3_concurrency,
        common.cache_mem_mib,
        common.cache_chunk_size,
    );
    let cache_fetch_size = common.cache_fetch_size.unwrap_or(cache_chunk_size);
    let cache_dir = common.cache_dir.clone();
    let cache_trace_log = common.cache_trace_log.clone();
    let path = image_path.clone();
    match format {
        Format::E01 => {
            let cache_mode = if common.metadata_cache {
                E01CacheMode::DualHybrid {
                    content_disk_mib: common.content_cache_disk_mib,
                    metadata_mem_mib: common.metadata_cache_mem_mib,
                    metadata_disk_mib: common.metadata_cache_disk_mib,
                    regular_phase: make_cache_phase(&common)?,
                }
            } else {
                E01CacheMode::SingleMemory
            };
            run_serve(
                common,
                &image_path,
                move || {
                    open_e01(
                        &path,
                        ignore_checksums,
                        readahead,
                        s3_concurrency,
                        cache_mem_mib,
                        cache_mode,
                        cache_dir,
                        cache_chunk_size,
                        cache_fetch_size,
                        cache_trace_log.as_deref(),
                    )
                },
                io_log,
            )
        }
        Format::Vmdk => {
            let cache_mode = if common.metadata_cache {
                VmdkCacheMode::DualHybrid {
                    content_disk_mib: common.content_cache_disk_mib,
                    metadata_mem_mib: common.metadata_cache_mem_mib,
                    metadata_disk_mib: common.metadata_cache_disk_mib,
                    regular_phase: make_cache_phase(&common)?,
                }
            } else {
                VmdkCacheMode::SingleMemory
            };
            run_serve(
                common,
                &image_path,
                move || {
                    open_vmdk(
                        &path,
                        readahead,
                        s3_concurrency,
                        cache_mem_mib,
                        cache_mode,
                        cache_dir,
                        cache_chunk_size,
                        cache_fetch_size,
                        cache_trace_log.as_deref(),
                    )
                },
                io_log,
            )
        }
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_e01_lowercase() {
        assert_eq!(detect_format("/data/image.e01").unwrap(), Format::E01);
    }

    #[test]
    fn detects_e01_uppercase() {
        assert_eq!(detect_format("/data/image.E01").unwrap(), Format::E01);
    }

    #[test]
    fn detects_vmdk() {
        assert_eq!(detect_format("/data/image.vmdk").unwrap(), Format::Vmdk);
    }

    #[test]
    fn detects_vmdk_uppercase() {
        assert_eq!(detect_format("/data/image.VMDK").unwrap(), Format::Vmdk);
    }

    #[test]
    fn detects_format_from_s3_url() {
        assert_eq!(
            detect_format("s3://bucket/cases/image.E01").unwrap(),
            Format::E01
        );
    }

    #[test]
    fn rejects_unknown_extension() {
        let err = detect_format("/data/image.raw").unwrap_err();
        assert!(err.contains(".e01 or .vmdk"), "unexpected error: {err}");
    }

    #[test]
    fn rejects_missing_extension() {
        let err = detect_format("/data/image").unwrap_err();
        assert!(err.contains(".e01 or .vmdk"), "unexpected error: {err}");
    }

    #[test]
    fn cache_dir_flag_parses_to_some() {
        let args = Args::try_parse_from([
            "diskimage-nbd",
            "/data/image.e01",
            "--cache-dir",
            "/mnt/nvme-cache",
        ])
        .unwrap();
        assert_eq!(
            args.common.cache_dir,
            Some(std::path::PathBuf::from("/mnt/nvme-cache"))
        );
    }

    #[test]
    fn cache_dir_flag_defaults_to_none() {
        let args = Args::try_parse_from(["diskimage-nbd", "/data/image.e01"]).unwrap();
        assert_eq!(args.common.cache_dir, None);
    }
}
