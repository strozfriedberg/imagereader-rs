//! Serve an E01 or VMDK image over NBD (fixed new-style), similar to `qemu-nbd`.
//! The image format (E01 vs VMDK) is chosen by the input path's extension.

use clap::Parser;
use diskimage_nbd::{
    CommonArgs, NbdImage, init_tracing, make_cache_phase, run_serve, server::open_io_log,
};
use e01::e01_reader::{
    CacheMode as E01CacheMode, CorruptChunkPolicy, CorruptSectionPolicy, E01Reader,
    E01ReaderOptions,
};
use std::{io, path::Path, process::ExitCode};
use vmdkrs::vmdk_reader::{CacheMode as VmdkCacheMode, VmdkReader, VmdkReaderOptions};

#[derive(Parser)]
#[command(author, version, about = "Serve an E01 or VMDK image over NBD", long_about = None)]
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

fn open_e01(
    path: &str,
    ignore_checksums: bool,
    readahead: usize,
    s3_concurrency: usize,
    cache_mem_mib: usize,
    cache_mode: E01CacheMode,
) -> Result<Adapter, Box<dyn std::error::Error>> {
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
    .map(|r| Adapter::E01(E01Adapter(r)))
    .map_err(Into::into)
}

fn open_vmdk(
    path: &str,
    readahead: usize,
    s3_concurrency: usize,
    cache_mem_mib: usize,
    cache_mode: VmdkCacheMode,
) -> Result<Adapter, Box<dyn std::error::Error>> {
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
    let (readahead, s3_concurrency, cache_mem_mib) = (
        common.readahead,
        common.s3_concurrency,
        common.cache_mem_mib,
    );
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
                move || open_vmdk(&path, readahead, s3_concurrency, cache_mem_mib, cache_mode),
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
}
