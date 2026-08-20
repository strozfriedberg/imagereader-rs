//! Serve an E01, VMDK or raw image over NBD (fixed new-style), similar to
//! `qemu-nbd`. The image format is chosen by the input path's extension.

use clap::Parser;
use diskimage_nbd::{
    CommonArgs, NbdImage, init_tracing, make_cache_phase, run_serve, server::open_io_log,
};
use e01::IoLog as E01IoLog;
use e01::e01_reader::{
    CacheMode as E01CacheMode, CorruptChunkPolicy, CorruptSectionPolicy, E01Reader,
    E01ReaderOptions,
};
use rawdisk::IoLog as RawIoLog;
use rawdisk::rawdisk_reader::{CacheMode as RawCacheMode, RawdiskReader, RawdiskReaderOptions};
use std::{io, path::PathBuf, process::ExitCode, sync::Arc};
use vmdkrs::IoLog as VmdkIoLog;
use vmdkrs::vmdk_reader::{CacheMode as VmdkCacheMode, VmdkReader, VmdkReaderOptions};

#[derive(Parser)]
// long_version (shown by `--version`) adds the build commit; `-V` stays plain.
#[command(
    author,
    version,
    long_version = buildinfo::long_version!(),
    about = "Serve an E01, VMDK or raw image over NBD",
    long_about = None
)]
struct Args {
    /// Path to an E01 segment, a VMDK descriptor/image, or a raw image
    /// (local path, glob, or s3:// URL). Format is chosen by extension:
    /// .e01 -> E01, .vmdk -> VMDK, .raw/.dd/.img or a numbered segment
    /// (disk.001) -> raw. Naming any segment of a split raw image opens the
    /// whole image.
    image_path: String,

    /// Ignore chunk checksums while reading (E01 only; silently has no effect
    /// for VMDK or raw, which have no per-chunk checksums).
    #[arg(short, long)]
    ignore_checksums: bool,

    #[command(flatten)]
    common: CommonArgs,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Format {
    E01,
    Vmdk,
    Raw,
}

/// The suffix after the final `.` of the last path component, or `None` when
/// there is no dot or nothing follows it.
///
/// Deliberately not `Path::extension()`, which reports `None` for a name that
/// begins with a dot. Every reader here splits on the final `.` instead --
/// e01's `validate_proto_extension` and rawdisk's `split_numeric_suffix` both
/// do -- so `Path::extension()` made this function reject names the readers
/// would have opened without complaint: `/img/.001` is a segment like any
/// other, and `/img/.e01` an E01 like any other.
fn final_suffix(path: &str) -> Option<&str> {
    let name = path.rsplit(['/', '\\']).next().unwrap_or(path);
    name.rsplit_once('.')
        .map(|(_, suffix)| suffix)
        .filter(|suffix| !suffix.is_empty())
}

fn detect_format(path: &str) -> Result<Format, String> {
    let ext = final_suffix(path).map(|ext| ext.to_ascii_lowercase());

    match ext.as_deref() {
        Some("e01") => Ok(Format::E01),
        Some("vmdk") => Ok(Format::Vmdk),
        Some("raw" | "dd" | "img") => Ok(Format::Raw),
        // A numeric suffix is a split raw segment; the reader finds the rest.
        // Left broad on purpose. Narrowing it to something more segment-shaped
        // -- zero-padded, or below some bound -- would reject `disk.150`, which
        // is a perfectly ordinary way to name the segment you happen to have,
        // and rawdisk rewinds to the start of the sequence from any of them. A
        // false positive here costs nothing: raw means "serve these bytes", so
        // an unrecognized file is served as-is rather than misparsed.
        Some(s) if s.bytes().all(|b| b.is_ascii_digit()) => Ok(Format::Raw),
        _ => Err(format!(
            "unsupported image extension in {path:?}; expected .e01, .vmdk, .raw, .dd, .img, or a numbered segment"
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

struct RawAdapter(RawdiskReader);

impl NbdImage for RawAdapter {
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
    Raw(RawAdapter),
}

impl NbdImage for Adapter {
    fn size(&self) -> u64 {
        match self {
            Adapter::E01(a) => a.size(),
            Adapter::Vmdk(a) => a.size(),
            Adapter::Raw(a) => a.size(),
        }
    }

    fn read_at_offset(&mut self, offset: u64, buf: &mut [u8]) -> io::Result<usize> {
        match self {
            Adapter::E01(a) => a.read_at_offset(offset, buf),
            Adapter::Vmdk(a) => a.read_at_offset(offset, buf),
            Adapter::Raw(a) => a.read_at_offset(offset, buf),
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
    io_log: Option<Arc<E01IoLog>>,
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
            // server -- see docs/perf-notes.md -- but keep behavior unchanged here.
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
    io_log: Option<Arc<VmdkIoLog>>,
) -> Result<Adapter, Box<dyn std::error::Error>> {
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

#[allow(clippy::too_many_arguments)]
fn open_raw(
    path: &str,
    readahead: usize,
    s3_concurrency: usize,
    cache_mem_mib: usize,
    cache_mode: RawCacheMode,
    cache_dir: Option<PathBuf>,
    cache_chunk_size: usize,
    cache_fetch_size: usize,
    io_log: Option<Arc<RawIoLog>>,
) -> Result<Adapter, Box<dyn std::error::Error>> {
    RawdiskReader::open_with_options(
        path,
        &RawdiskReaderOptions {
            foyer_readahead: readahead,
            s3_concurrency,
            cache_mem_mib,
            cache_mode,
            cache_dir,
            io_log,
            cache_chunk_size,
            cache_fetch_size,
        },
    )
    .map(|r| Adapter::Raw(RawAdapter(r)))
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
                move || -> Result<Adapter, Box<dyn std::error::Error>> {
                    let trace = cache_trace_log.as_deref().map(E01IoLog::open).transpose()?;
                    let adapter = open_e01(
                        &path,
                        ignore_checksums,
                        readahead,
                        s3_concurrency,
                        cache_mem_mib,
                        cache_mode,
                        cache_dir,
                        cache_chunk_size,
                        cache_fetch_size,
                        trace.clone(),
                    )?;
                    // The reader's trace log starts suppressed so that opening the
                    // image -- reading section headers and the chunk table -- does
                    // not swamp the served workload. Nothing else ever flipped it,
                    // so the file stayed empty.
                    if let Some(trace) = &trace {
                        trace.begin_serving()?;
                    }
                    Ok(adapter)
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
                move || -> Result<Adapter, Box<dyn std::error::Error>> {
                    let trace = cache_trace_log
                        .as_deref()
                        .map(VmdkIoLog::open)
                        .transpose()?;
                    let adapter = open_vmdk(
                        &path,
                        readahead,
                        s3_concurrency,
                        cache_mem_mib,
                        cache_mode,
                        cache_dir,
                        cache_chunk_size,
                        cache_fetch_size,
                        trace.clone(),
                    )?;
                    if let Some(trace) = &trace {
                        trace.begin_serving()?;
                    }
                    Ok(adapter)
                },
                io_log,
            )
        }
        Format::Raw => {
            let cache_mode = if common.metadata_cache {
                RawCacheMode::DualHybrid {
                    content_disk_mib: common.content_cache_disk_mib,
                    metadata_mem_mib: common.metadata_cache_mem_mib,
                    metadata_disk_mib: common.metadata_cache_disk_mib,
                    regular_phase: make_cache_phase(&common)?,
                }
            } else {
                RawCacheMode::SingleMemory
            };
            run_serve(
                common,
                &image_path,
                move || -> Result<Adapter, Box<dyn std::error::Error>> {
                    let trace = cache_trace_log.as_deref().map(RawIoLog::open).transpose()?;
                    let adapter = open_raw(
                        &path,
                        readahead,
                        s3_concurrency,
                        cache_mem_mib,
                        cache_mode,
                        cache_dir,
                        cache_chunk_size,
                        cache_fetch_size,
                        trace.clone(),
                    )?;
                    if let Some(trace) = &trace {
                        trace.begin_serving()?;
                    }
                    Ok(adapter)
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
        let err = detect_format("/data/image.qcow2").unwrap_err();
        assert!(
            err.contains("unsupported image extension"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn rejects_missing_extension() {
        let err = detect_format("/data/image").unwrap_err();
        assert!(
            err.contains("unsupported image extension"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn detects_raw_extensions() {
        for path in ["/img/d.raw", "/img/d.dd", "/img/d.img", "/img/d.001"] {
            assert_eq!(detect_format(path).unwrap(), Format::Raw, "{path}");
        }
    }

    #[test]
    fn still_detects_e01_and_vmdk() {
        assert_eq!(detect_format("/img/d.E01").unwrap(), Format::E01);
        assert_eq!(detect_format("/img/d.vmdk").unwrap(), Format::Vmdk);
    }

    /// `Path::extension()` reports None for a leading-dot filename, so these
    /// were rejected here despite the readers opening them without complaint --
    /// both split on the final `.` instead. The CLI must not refuse to serve an
    /// image the reader supports.
    #[test]
    fn detects_dotfile_names_the_readers_accept() {
        assert_eq!(detect_format("/img/.001").unwrap(), Format::Raw);
        assert_eq!(detect_format("/img/.e01").unwrap(), Format::E01);
        assert_eq!(detect_format("/img/.vmdk").unwrap(), Format::Vmdk);
        assert_eq!(detect_format(".001").unwrap(), Format::Raw);
    }

    /// A dot in a directory name is not a suffix. Only the last component counts.
    #[test]
    fn a_dot_in_a_directory_is_not_a_suffix() {
        assert!(detect_format("/img/v1.2/disk").is_err());
        assert_eq!(detect_format("/img/v1.2/disk.001").unwrap(), Format::Raw);
        assert_eq!(
            detect_format("/img/case.2024/disk.raw").unwrap(),
            Format::Raw
        );
    }

    /// A trailing dot leaves no suffix at all.
    #[test]
    fn a_trailing_dot_is_not_a_suffix() {
        assert!(detect_format("/img/disk.").is_err());
    }

    /// The numeric rule is deliberately broad: any all-digit suffix is a raw
    /// segment. Narrowing it to something more segment-shaped would reject
    /// `disk.150`, an ordinary way to name the segment you have. Pinned so the
    /// breadth stays a decision rather than an accident.
    #[test]
    fn any_all_digit_suffix_is_raw() {
        for path in ["/img/d.1", "/img/d.150", "/img/d.0000", "/img/backup.2024"] {
            assert_eq!(detect_format(path).unwrap(), Format::Raw, "{path}");
        }
    }

    #[test]
    fn rejects_unknown_extensions() {
        assert!(detect_format("/img/d.qcow2").is_err());
        assert!(detect_format("/img/d").is_err());
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
