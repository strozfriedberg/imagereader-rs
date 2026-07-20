use byteorder::{LittleEndian, ReadBytesExt};
use std::{
    collections::HashMap,
    io::{BufReader, Read, Seek, SeekFrom},
    sync::Arc,
};
use tokio::runtime::Runtime;
use url::Url;

use crate::{
    errors::{OpenError, OpenErrorKind},
    extent_description::{ExtentDescription, ExtentDescriptionInner},
    header::{VmdkSeSparseMeta, VmdkSparseMeta, read_header_sesparse, read_header_sparse},
    storage::{ExtentStorage, FlatStorage, ReadSeekSource, SparseStorage},
    vmdk_reader::source_for_url,
};
use imagesource::{Cache, CacheReadSeek, IoLog, ReadSeek, s3_creds::S3Auth};

/*
RW 8323072 FLAT "CentOS 3-f001.vmdk" 0
RW 2162688 FLAT "CentOS 3-f002.vmdk" 0

sector_start = 0, sectors = 8323072
sector_start = 8323072, sectors = 2162688
*/

#[derive(Debug)]
pub struct Extent {
    pub start_sector: u64,
    pub sectors: u64,
    pub storage: ExtentStorage,
}

impl Extent {
    pub fn spans(&self) -> impl Iterator<Item = (u64, u64)> {
        match &self.storage {
            // Sparse storage is a collection of blocks of bytes.
            // It need not cover the extent's whole space. The grain table
            // comes from the sparse file's own header, which can describe
            // grains past the descriptor-declared end of the extent; clamp
            // so those never shadow the next extent's sectors.
            ExtentStorage::Sparse(storage) => {
                let extent_end = self.start_sector + self.sectors;
                storage
                    .grain_table
                    .keys()
                    .filter_map(|goff| {
                        // grain_size is in sectors
                        let beg = self.start_sector + goff * storage.grain_size;
                        if beg >= extent_end {
                            return None;
                        }
                        let end = (beg + storage.grain_size).min(extent_end);
                        Some((beg, end))
                    })
                    .collect::<Vec<_>>()
            }
            // Flat and Zero storage are each a single block of bytes.
            ExtentStorage::Flat(_) | ExtentStorage::Zero => {
                vec![(self.start_sector, self.start_sector + self.sectors)]
            }
        }
        .into_iter()
    }

    pub fn has_file(&self) -> bool {
        !matches!(self.storage, ExtentStorage::Zero)
    }
}

const SECTOR_SIZE: u64 = 512;

fn read_grain_table_sparse<R>(
    h: &VmdkSparseMeta,
    src: &mut R,
) -> Result<HashMap<u64, u64>, std::io::Error>
where
    R: Read + Seek,
{
    // read level 1
    src.seek(SeekFrom::Start(h.l1_offset))?;

    let l1_entries = (0..h.l1_len)
        .map(|_| {
            src.read_u32::<LittleEndian>()
                .map(|e| e as u64 * SECTOR_SIZE)
        })
        .collect::<Result<Vec<u64>, std::io::Error>>()?;

    // read level 2. Keys are grain indices within this extent (0-based).
    let mut grain_table = HashMap::new();
    let total_grains = h.sectors.div_ceil(h.cluster_sectors);
    let mut cur_grain = 0u64;

    for l2_offset in l1_entries {
        if cur_grain >= total_grains {
            // we've mapped every grain; stop
            break;
        }

        let l2_len = h.l2_len.min(total_grains - cur_grain);

        if l2_offset == 0 {
            // the data for this entry is in the parent
            cur_grain += l2_len;
            continue;
        }

        src.seek(SeekFrom::Start(l2_offset))?;

        let l2_entries = (0..l2_len)
            .map(|_| src.read_u32::<LittleEndian>().map(|e| e as u64))
            .collect::<Result<Vec<u64>, std::io::Error>>()?;

        grain_table.extend(
            l2_entries
                .iter()
                .enumerate()
                .filter(|(_, grain)| **grain != 0)
                .map(|(i, grain)| (cur_grain + i as u64, *grain)),
        );

        cur_grain += l2_len;
    }

    Ok(grain_table)
}

fn read_grain_table_sesparse<R>(
    h: &VmdkSeSparseMeta,
    src: &mut R,
) -> Result<HashMap<u64, u64>, std::io::Error>
where
    R: Read + Seek,
{
    /*
        SESPARSE extents differ from earlier sparse extent types:

            * table entries are 8 bytes instead of 4
            * l1 entries are rather baroque; see below for how they're read
            * l1 entries contain indices into the table of l2 tables, instead
              of offsets to l2 tables

        The only available reference implementation is QEMU's:

            https://github.com/qemu/qemu/blob/master/block/vmdk.c

        We've tried to document which values have which units.
    */

    // read level 1
    src.seek(SeekFrom::Start(h.l1_offset))?;

    let l1_entries = (0..h.l1_len)
        .map(|_| src.read_u64::<LittleEndian>())
        .collect::<Result<Vec<u64>, std::io::Error>>()?;

    // read level 2. Keys are grain indices within this extent (0-based).
    let mut grain_table = HashMap::new();
    let total_grains = h.sectors.div_ceil(h.cluster_sectors);
    let mut cur_grain = 0u64;

    // size in bytes of an l2 table
    let l2_size = h.l2_len * 8;

    for l1_entry in l1_entries {
        if cur_grain >= total_grains {
            // we've mapped every grain; stop
            break;
        }

        let l2_len = h.l2_len.min(total_grains - cur_grain);

        // high nibble of l1 entries are 0 (unallocated) or 1 (allocated)

        if l1_entry == 0 {
            // Thank you Mario! But our princess is in another castle!
            // (the data for this entry is in the parent)
            cur_grain += l2_len;
            continue;
        }

        if l1_entry & 0xF000000000000000 != 0x1000000000000000 {
            return Err(std::io::Error::other("bad l1 entry"));
        }

        let l2_index = l1_entry & 0x0FFFFFFFFFFFFFFF;
        let l2_offset = h.l2_tables_offset + l2_index * l2_size;

        src.seek(SeekFrom::Start(l2_offset))?;

        let l2_entries = (0..l2_len)
            .map(|_| src.read_u64::<LittleEndian>())
            .collect::<Result<Vec<u64>, std::io::Error>>()?;

        for (i, &l2_entry) in l2_entries.iter().enumerate() {
            if l2_entry == 0 {
                // the data for this entry is in the parent
                continue;
            }

            // cluster_offset is in sectors
            let cluster_offset = match l2_entry & 0xF000000000000000 {
                0x1000000000000000 | 0x2000000000000000 => {
                    // zeroed grain
                    1
                }
                0x3000000000000000 => {
                    // allocted grain
                    h.clusters_offset
                        + (((l2_entry & 0x0FFF000000000000) >> 48)
                            | ((l2_entry & 0x0000FFFFFFFFFFFF) << 12))
                            * h.cluster_sectors
                }
                _ => {
                    // 0 in high nibble means unallocated grain, which
                    // should not happen; anything else is also corrupt
                    return Err(std::io::Error::other("bad l2 entry"));
                }
            };

            grain_table.insert(cur_grain + i as u64, cluster_offset);
        }

        cur_grain += l2_len;
    }

    Ok(grain_table)
}

fn read_extent<R, F>(
    ed: &ExtentDescription,
    start_sector: u64,
    filename: F,
    src: R,
) -> Result<ExtentStorage, OpenError>
where
    R: ReadSeek + Clone + std::fmt::Debug + Sync + 'static,
    F: Into<String>,
{
    let filename = filename.into();

    Ok(match &ed.kind {
        ExtentDescriptionInner::Sparse { .. } | ExtentDescriptionInner::VmfsSparse { .. } => {
            let header = read_header_sparse(src.clone())?;
            let mut buffered = BufReader::with_capacity(1024 * 1024, src.clone());
            let grain_table = read_grain_table_sparse(&header, &mut buffered)?;

            ExtentStorage::Sparse(SparseStorage {
                source: Box::new(src) as Box<dyn ReadSeekSource>,
                filename,
                grain_table,
                grain_size: header.cluster_sectors,
                has_compressed_grain: header.compressed,
                zeroed_grain_table_entry: header.has_zero_grain,
                start_sector,
            })
        }
        ExtentDescriptionInner::SeSparse { .. } => {
            let header = read_header_sesparse(src.clone())?;
            let mut buffered = BufReader::with_capacity(1024 * 1024, src.clone());
            let grain_table = read_grain_table_sesparse(&header, &mut buffered)?;

            ExtentStorage::Sparse(SparseStorage {
                source: Box::new(src) as Box<dyn ReadSeekSource>,
                filename,
                grain_table,
                grain_size: header.cluster_sectors,
                has_compressed_grain: false,
                zeroed_grain_table_entry: true,
                start_sector,
            })
        }
        ExtentDescriptionInner::Vmfs { .. } => ExtentStorage::Flat(FlatStorage {
            source: Box::new(src) as Box<dyn ReadSeekSource>,
            filename,
            offset: 0,
            start_sector,
        }),
        ExtentDescriptionInner::Flat { offset, .. } => ExtentStorage::Flat(FlatStorage {
            source: Box::new(src) as Box<dyn ReadSeekSource>,
            filename,
            offset: *offset,
            start_sector,
        }),
        _ => todo!("TODO: {:?} support", ed.kind),
    })
}

#[allow(clippy::too_many_arguments)]
pub fn read_extents(
    image_url: &Url,
    eds: &[ExtentDescription],
    is_bin_and_singular: bool,
    cache: Arc<dyn Cache>,
    runtime: Arc<Runtime>,
    mut idx: usize,
    s3_auth: Option<&Arc<S3Auth>>,
    io_log: Option<&Arc<IoLog>>,
) -> Result<Vec<Extent>, OpenError> {
    let mut extents = vec![];

    let mut start_sector = 0;

    for ed in eds {
        let filename = ed.filename();

        let ed_url = image_url
            .join(filename)
            .map_err(|_| OpenErrorKind::BadPath(filename.into()))
            .map_err(OpenError::from)
            .map_err(|e| e.with_path(filename))?;

        let src = source_for_url(&ed_url, idx, &runtime, s3_auth, io_log).or_else(|e|
                // if first filename is wrong and we are bin, try current file
                if is_bin_and_singular && &ed_url != image_url {
                    source_for_url(image_url, idx, &runtime, s3_auth, io_log)
                }
                else {
                    Err(e)
                }
            )?;

        let seg_len = src.end();

        cache.add_source(idx, src);

        let crs = CacheReadSeek::new(
            cache.clone(),
            runtime.clone(),
            idx,
            seg_len,
            io_log.cloned(),
        );

        let storage =
            read_extent(ed, start_sector, filename, crs).map_err(|e| e.with_path(ed_url))?;

        extents.push(Extent {
            sectors: ed.sectors,
            start_sector,
            storage,
        });

        start_sector += ed.sectors;
        idx += 1;
    }

    Ok(extents)
}

#[cfg(test)]
mod test {
    use super::*;
    use crate::storage::SparseStorage;
    use std::io::Cursor;

    /// A sparse extent's own header (not the descriptor) determines which
    /// grains exist, so a corrupt or grain-rounded header can describe grains
    /// past the descriptor-declared end of the extent. Those spans must not
    /// leak into the next extent's sector range.
    #[test]
    fn sparse_spans_are_clamped_to_the_extent_range() {
        // extent covers sectors [100, 112); grain size is 8 sectors.
        // grain 0 => [100, 108), grain 1 => [108, 116) overhangs the end,
        // grain 2 => [116, 124) lies entirely past the end.
        let extent = Extent {
            start_sector: 100,
            sectors: 12,
            storage: ExtentStorage::Sparse(SparseStorage {
                source: Box::new(Cursor::new(vec![0u8; 4096])),
                filename: "crafted.vmdk".into(),
                grain_table: HashMap::from([(0, 1), (1, 2), (2, 3)]),
                grain_size: 8,
                has_compressed_grain: false,
                zeroed_grain_table_entry: false,
                start_sector: 100,
            }),
        };

        let mut spans: Vec<_> = extent.spans().collect();
        spans.sort();
        assert_eq!(spans, vec![(100, 108), (108, 112)]);
    }
}
