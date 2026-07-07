use s3::{bucket::Bucket, region::Region};
use std::{
    collections::BTreeMap,
    fmt::Debug,
    io::{self, Seek, SeekFrom},
    path::{Path, PathBuf},
    str::FromStr,
    sync::{Arc, atomic::AtomicBool},
};
use tokio::runtime::Runtime;
use tracing::debug;
use url::Url;

use crate::{
    bytessource::BytesSource,
    cache::Cache,
    cachereadseek::CacheReadSeek,
    descriptor::{extract_parent_fn_hint, read_descriptor_file, read_descriptor_internal},
    errors::{DescriptorError, InitError, OpenError, OpenErrorKind},
    extent_description::extract_extent_descriptions,
    extents::{Extent, read_extents},
    filesource::FileSource,
    foyercache::FoyerCache,
    header::{FileType, Vmdk4Header, check_signature},
    io_log::IoLog,
    s3_creds::{S3Auth, resolve_s3_auth, s3_region_name, snapshot_credentials_sync},
    s3source::S3Source,
    spans::{insert_span, remove_span},
    storage::ExtentStorage,
};

const SECTOR_SIZE: u64 = 512;

pub struct VmdkReader {
    pub image_path: PathBuf,
    pub image_size: u64,

    spans: Vec<(u64, (u64, usize))>,
    extents: Vec<Extent>,
    #[allow(dead_code)]
    cache: Arc<dyn Cache>,
    #[allow(dead_code)]
    runtime: Arc<Runtime>,
}

impl Debug for VmdkReader {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("VmdkReader")
            .field("image_path", &self.image_path)
            .field("image_size", &self.image_size)
            .field("spans", &self.spans)
            .finish()
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ReadError {
    #[error("Requested offset {0} is beyond end of image {1}")]
    OffsetBeyondEnd(u64, u64),
    #[error("Offset {0} not found")]
    OffsetNotFound(u64),
    #[error("{0}")]
    IoError(#[from] io::Error),
}

#[derive(Debug, thiserror::Error)]
pub enum VmdkError {
    #[error("{0}")]
    OpenError(#[from] OpenError),
    #[error("{0}")]
    ReadError(#[from] ReadError),
}

fn path_or_url_to_url<P: AsRef<str>>(p: P) -> Option<Url> {
    match Url::parse(p.as_ref()) {
        // might be a path; make it absolute and reparse
        Err(url::ParseError::RelativeUrlWithoutBase) => Path::new(p.as_ref())
            .canonicalize()
            .map(Url::from_file_path)
            .map_err(|_| ())
            // FIXME: use flatten after Rust 1.89
            //            .flatten()
            .and_then(|r| r)
            .ok(),
        r => r.ok(),
    }
}

fn s3_region_for_host_in_region(name: &str, region_name: &str) -> Region {
    if name.ends_with("-s3alias") || name.ends_with("-ext-s3alias") {
        Region::Custom {
            region: region_name.to_string(),
            endpoint: format!("s3-accesspoint.{region_name}.amazonaws.com"),
        }
    } else {
        Region::from_str(region_name).unwrap_or(Region::UsEast1)
    }
}

fn s3_region_configured(auth: &S3Auth) -> bool {
    s3_region_name(Some(auth)).is_some()
}

fn s3_region_for_host(name: &str, auth: Option<&S3Auth>) -> Region {
    // `us-east-1` here is only the bootstrap endpoint used to issue the
    // GetBucketLocation discovery call when no region is configured; it is not a
    // regional default. A configured region (env/profile) is used as-is, and an
    // unconfigured bucket's real region is discovered in `s3_bucket`.
    let region_name = s3_region_name(auth).unwrap_or_else(|| "us-east-1".to_string());
    s3_region_for_host_in_region(name, &region_name)
}

fn s3_bucket(
    name: &str,
    ctx: &str,
    runtime: &Runtime,
    auth: &Arc<S3Auth>,
) -> Result<Bucket, OpenError> {
    let region = s3_region_for_host(name, Some(auth));
    let credentials = snapshot_credentials_sync(runtime, auth)
        .map_err(OpenError::from)
        .map_err(|e| e.with_path(ctx))?;

    let bucket = Bucket::new(name, region, credentials)
        .map(|b| *b)
        .map_err(std::io::Error::other)
        .map_err(OpenError::from)
        .map_err(|e| e.with_path(ctx))?;

    // A configured region (env/profile) or an access-point alias is trusted as-is.
    // Otherwise discover the bucket's real region instead of assuming one.
    if s3_region_configured(auth) || name.ends_with("-s3alias") || name.ends_with("-ext-s3alias") {
        return Ok(bucket);
    }

    match runtime.block_on(bucket.location()) {
        Ok((actual, _)) if actual != bucket.region() => {
            let credentials = snapshot_credentials_sync(runtime, auth)
                .map_err(OpenError::from)
                .map_err(|e| e.with_path(ctx))?;
            Bucket::new(name, actual, credentials)
                .map(|b| *b)
                .map_err(std::io::Error::other)
                .map_err(OpenError::from)
                .map_err(|e| e.with_path(ctx))
        }
        Ok(_) | Err(_) => Ok(bucket),
    }
}

pub fn source_for_url(
    url: &Url,
    segment: usize,
    runtime: &Runtime,
    s3_auth: Option<&Arc<S3Auth>>,
    io_log: Option<&Arc<IoLog>>,
) -> Result<Box<dyn BytesSource + Send + Sync>, OpenError> {
    match url.scheme() {
        "file" => {
            let p = if cfg!(windows) {
                // Windows file URLs get a spare / before the drive letter,
                // which we have to remove when using it as a path.
                url.path().trim_start_matches('/')
            } else {
                url.path()
            };

            let len = std::fs::metadata(p)
                .map_err(OpenError::from)
                .map_err(|e| e.with_path(p))?
                .len();
            Ok(Box::new(FileSource {
                path: p.into(),
                len,
            }))
        }
        "s3" => {
            let auth = s3_auth.ok_or_else(|| {
                OpenError::from(std::io::Error::other("S3 credentials not resolved"))
            })?;
            let name = url
                .host_str()
                .ok_or(OpenErrorKind::BadPath(url.to_string()))?;
            let key = url.path().trim_start_matches('/');
            let bucket = s3_bucket(name, url.as_ref(), runtime, auth)?;

            let (h, _) = runtime
                .block_on(bucket.head_object(key))
                .map_err(std::io::Error::other)
                .map_err(OpenError::from)
                .map_err(|e| e.with_path(url))?;

            let len = h.content_length.unwrap().try_into().unwrap();
            debug!("content-length: {len}");

            Ok(Box::new(S3Source::new(
                bucket,
                key.to_string(),
                len,
                auth.clone(),
                segment,
                io_log.cloned(),
            )))
        }
        _ => Err(OpenErrorKind::UnsupportedScheme(url.to_string()).into()),
    }
}

fn handle_image(
    current_url: &Url,
    mut idx: usize,
    cache: Arc<dyn Cache>,
    runtime: Arc<Runtime>,
    s3_auth: Option<&Arc<S3Auth>>,
    io_log: Option<&Arc<IoLog>>,
) -> Result<(Vec<Extent>, Option<Url>), OpenError> {
    let src = source_for_url(current_url, idx, &runtime, s3_auth, io_log)?;
    let seg_len = src.end();

    cache.add_source(idx, src);

    let mut crs = CacheReadSeek::new(cache.clone(), runtime.clone(), idx, seg_len, io_log.cloned());

    idx += 1;

    // determine what we're reading
    let ft = check_signature(&mut crs)?;

    // get the descriptor
    let descriptor = match ft {
        // this has an internal descriptor
        Some(FileType::Vmdk4) => {
            crs.seek(SeekFrom::Start(0))?;
            let mut h = Vmdk4Header::from_reader(&mut crs)?;

            if h.use_secondary() {
                crs.seek(SeekFrom::End(-1024))?;
                h = Vmdk4Header::from_reader(&mut crs)?;
            }

            if h.desc_offset > 0 {
                read_descriptor_internal(&mut crs, h.desc_offset)?
            } else {
                "".into()
            }
        }
        // this is a descriptor file
        None => {
            crs.seek(SeekFrom::Start(0))?;
            read_descriptor_file(&mut crs)?
        }
        // this is bogus
        _ => return Err(DescriptorError::ParseExtentDescriptionError.into()),
    };

    // get the extent descriptions
    let eds = extract_extent_descriptions(&descriptor)
        .or(Err(DescriptorError::ParseExtentDescriptionError))?;

    let is_bin_and_singular = ft == Some(FileType::Vmdk4) && eds.len() == 1;

    // read each extent
    let extents = read_extents(
        current_url,
        &eds,
        is_bin_and_singular,
        cache.clone(),
        runtime.clone(),
        idx,
        s3_auth,
        io_log,
    )?;

    // find the parent image, if any
    let parent_url = extract_parent_fn_hint(&descriptor)
        .map(|p| current_url.join(&p).map_err(|_| OpenErrorKind::BadPath(p)))
        .transpose()?;

    Ok((extents, parent_url))
}

pub const DEFAULT_CACHE_MEM_MIB: usize = 256;
pub const DEFAULT_S3_CONCURRENCY: usize = 8;
pub const DEFAULT_CACHE_CHUNK_SIZE: usize = 1024 * 1024;

/// How the foyer cache is structured for a session.
#[derive(Debug, Clone)]
pub enum CacheMode {
    /// Local-file backing: a single memory-only cache.
    SingleMemory,
    /// S3 backing: a dedicated metadata cache plus a content cache. `regular_phase`
    /// starts `false` (metadata phase) and is flipped to `true` by the SIGUSR1 handler.
    DualHybrid {
        content_disk_mib: usize,
        metadata_mem_mib: usize,
        metadata_disk_mib: usize,
        regular_phase: Arc<AtomicBool>,
    },
}

impl Default for CacheMode {
    fn default() -> Self {
        CacheMode::SingleMemory
    }
}

#[derive(Debug, Clone)]
pub struct VmdkReaderOptions {
    /// Foyer backing-cache readahead in chunks (S3/file fetch). 0 disables.
    pub foyer_readahead: usize,
    /// Max concurrent in-flight S3 byte-range fetches. 0 = serial.
    pub s3_concurrency: usize,
    /// Foyer in-memory cache capacity (see [`DEFAULT_CACHE_MEM_MIB`]).
    pub cache_mem_mib: usize,
    /// Cache structure for this session (single vs dedicated-metadata).
    pub cache_mode: CacheMode,
    /// Base directory for foyer's on-disk cache (created as a random subdir
    /// under this path). `None` uses the OS default temp directory.
    pub cache_dir: Option<PathBuf>,
    /// When set, append JSONL read/S3 traces (see [`IoLog`]).
    pub io_log: Option<Arc<IoLog>>,
    /// Foyer block size in bytes.
    pub cache_chunk_size: usize,
}

impl Default for VmdkReaderOptions {
    fn default() -> Self {
        Self {
            foyer_readahead: 0,
            s3_concurrency: DEFAULT_S3_CONCURRENCY,
            cache_mem_mib: DEFAULT_CACHE_MEM_MIB,
            cache_mode: CacheMode::default(),
            cache_dir: None,
            io_log: None,
            cache_chunk_size: DEFAULT_CACHE_CHUNK_SIZE,
        }
    }
}

impl VmdkReader {
    /// Open with default options (single memory-only cache). Used by the C API and
    /// `vmdkverify`; kept single-arg for backward compatibility.
    pub fn open<T: AsRef<str>>(image_path: T) -> Result<Self, OpenError> {
        Self::open_with_options(image_path, &VmdkReaderOptions::default())
    }

    pub fn open_with_options<T: AsRef<str>>(
        image_path: T,
        opts: &VmdkReaderOptions,
    ) -> Result<Self, OpenError> {
        let mut current_url = path_or_url_to_url(&image_path)
            .ok_or(OpenErrorKind::BadPath(image_path.as_ref().into()))?;

        let runtime = Arc::new(
            tokio::runtime::Runtime::new()
                .map_err(InitError::TokioRuntimeFailed)
                .map_err(OpenErrorKind::from)?,
        );

        let cache_chunk_size = opts.cache_chunk_size;
        let c = match opts.cache_mode.clone() {
            CacheMode::SingleMemory => runtime.block_on(FoyerCache::single_memory(
                cache_chunk_size,
                opts.cache_mem_mib,
                opts.foyer_readahead,
                opts.s3_concurrency,
                opts.cache_dir.as_deref(),
            )),
            CacheMode::DualHybrid {
                content_disk_mib,
                metadata_mem_mib,
                metadata_disk_mib,
                regular_phase,
            } => runtime.block_on(FoyerCache::dual_hybrid(
                cache_chunk_size,
                opts.cache_mem_mib,
                content_disk_mib,
                metadata_mem_mib,
                metadata_disk_mib,
                opts.foyer_readahead,
                opts.s3_concurrency,
                regular_phase,
                opts.cache_dir.as_deref(),
            )),
        }
        .map_err(InitError::CacheSetupFailed)
        .map_err(OpenErrorKind::from)?;

        let cache: Arc<dyn Cache> = Arc::new(c);

        // Resolve S3 credentials once if the image is S3-backed; the whole extent
        // chain resolves relative to the same scheme.
        let s3_auth = if current_url.scheme() == "s3" {
            Some(Arc::new(
                resolve_s3_auth(&runtime).map_err(OpenError::from)?,
            ))
        } else {
            None
        };
        let io_log = opts.io_log.clone();

        let mut idx = 0;
        let mut spans: BTreeMap<u64, (u64, usize)> = BTreeMap::new();
        let mut uncovered: BTreeMap<u64, u64> = BTreeMap::new();
        let mut extents = vec![];
        let mut image_size = None;

        let image_size = 'img_loop: loop {
            let (img_extents, parent_url) = handle_image(
                &current_url,
                idx,
                cache.clone(),
                runtime.clone(),
                s3_auth.as_ref(),
                io_log.as_ref(),
            )?;

            idx += 1;

            // size for all images must match
            let size = img_extents.iter().fold(0, |acc, i| acc + i.sectors) * SECTOR_SIZE;

            if image_size.is_none() {
                image_size = Some(size);
                let sec_end = size.div_ceil(SECTOR_SIZE);
                uncovered.insert(0, sec_end);
            } else if let Some(s) = image_size
                && s != size
            {
                return Err(OpenError {
                    path: current_url.as_ref().into(),
                    kind: OpenErrorKind::BadParentExtentDescriptorSize(s, size),
                });
            }

            // add the extents for this image to the span map
            for ex in img_extents {
                for (beg, end) in ex.spans() {
                    insert_span(beg, end, extents.len(), &mut spans);
                    remove_span(beg, end, &mut uncovered);
                }

                if ex.has_file() {
                    idx += 1;
                }

                extents.push(ex);

                // stop if we have extents for all spans
                if uncovered.is_empty() {
                    break 'img_loop size;
                }
            }

            // keep going if we are not at the end of the image chain
            let Some(parent_url) = parent_url else {
                break 'img_loop size;
            };
            current_url = parent_url;
        };

        // fill missing spans with zeros
        for (lb, ub) in uncovered {
            debug!("zero-filling uncovered span [{}, {})", lb, ub);

            let ex = Extent {
                start_sector: lb,
                sectors: ub - lb,
                storage: ExtentStorage::Zero,
            };

            insert_span(lb, ub, extents.len(), &mut spans);

            extents.push(ex);
        }

        // spans are in bytes from here onward
        let spans = spans
            .into_iter()
            .map(|(lb, (ub, i))| (lb * SECTOR_SIZE, (ub * SECTOR_SIZE, i)))
            .collect::<Vec<_>>();

        Ok(Self {
            image_path: image_path.as_ref().into(),
            image_size,
            spans,
            extents,
            cache,
            runtime,
        })
    }

    pub fn read_at_offset(
        &mut self,
        mut offset: u64,
        mut buf: &mut [u8],
    ) -> Result<usize, ReadError> {
        let beg = offset;

        // don't start reading past the end
        let image_end = self.image_size;
        if beg > image_end {
            return Err(ReadError::OffsetBeyondEnd(beg, self.image_size));
        }

        // limit the buffer to the image end
        if beg + buf.len() as u64 > image_end {
            buf = &mut buf[..(image_end - beg) as usize];
        }

        let end = beg + buf.len() as u64;

        let mut i = match self.spans.binary_search_by_key(&beg, |e| e.0) {
            Ok(i) => i,
            // 0 is impossible as an insertion point because
            // there must be a span staring at 0
            Err(0) => unreachable!(),
            Err(i) => i - 1,
        };

        while offset < end {
            let span = self.spans[i];
            let span_end = span.1.0;
            let r = ((span_end - offset) as usize).min(buf.len());
            let ex = &mut self.extents[span.1.1];

            let r = ex.storage.read(offset, &mut buf[..r])?;

            offset += r as u64;
            buf = &mut buf[r..];

            if offset >= span_end {
                // advance to the next span to read more
                i += 1;
            }
        }

        Ok((end - beg) as usize)
    }
}

#[cfg(test)]
mod test {
    use super::*;

    fn write(dir: &std::path::Path, name: &str, bytes: &[u8]) {
        std::fs::write(dir.join(name), bytes).unwrap();
    }

    #[test]
    fn read_multi_extent_flat() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "flat-a.vmdk", &[0xAAu8; 1024]); // 2 sectors
        write(dir.path(), "flat-b.vmdk", &[0xBBu8; 1024]); // 2 sectors
        write(
            dir.path(),
            "multi.vmdk",
            b"# Disk DescriptorFile\n# Extent description\n\
              RW 2 FLAT \"flat-a.vmdk\" 0\nRW 2 FLAT \"flat-b.vmdk\" 0\n",
        );

        let path = dir.path().join("multi.vmdk");
        let mut reader = VmdkReader::open(path.to_str().unwrap()).unwrap();
        assert_eq!(reader.image_size, 2048);

        let mut buf = vec![0u8; 2048];
        let n = reader.read_at_offset(0, &mut buf).unwrap();
        assert_eq!(n, 2048);
        assert!(buf[..1024].iter().all(|&b| b == 0xAA), "first extent");
        assert!(buf[1024..].iter().all(|&b| b == 0xBB), "second extent");
    }

    #[test]
    fn read_flat_extent_with_nonzero_offset_field() {
        let dir = tempfile::tempdir().unwrap();
        // 1 sector of padding, then 2 sectors of payload.
        let mut file = vec![0u8; 512];
        file.extend(std::iter::repeat_n(0xCC, 1024));
        write(dir.path(), "flat-c.vmdk", &file);
        write(
            dir.path(),
            "off.vmdk",
            b"# Disk DescriptorFile\n# Extent description\n\
              RW 2 FLAT \"flat-c.vmdk\" 1\n",
        );

        let path = dir.path().join("off.vmdk");
        let mut reader = VmdkReader::open(path.to_str().unwrap()).unwrap();
        assert_eq!(reader.image_size, 1024);

        let mut buf = vec![0u8; 1024];
        reader.read_at_offset(0, &mut buf).unwrap();
        assert!(
            buf.iter().all(|&b| b == 0xCC),
            "offset field must be added so the leading padding sector is skipped"
        );
    }
}
