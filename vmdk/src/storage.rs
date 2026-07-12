use byteorder::{BigEndian, LittleEndian, ReadBytesExt};
use flate2::read::DeflateDecoder;
use std::{
    collections::HashMap,
    io::{self, Read, SeekFrom},
};

use crate::vmdk_reader::ReadError;
use imagesource::ReadSeek;

const SECTOR_SIZE: u64 = 512;

#[derive(Debug)]
pub struct SparseStorage {
    pub file: Box<dyn ReadSeek>,
    #[allow(dead_code)]
    pub filename: String,
    pub grain_table: HashMap<u64 /*grain index in extent*/, u64 /*real sector in file*/>,
    // grain size in sectors; grain byte size is grain_size * 512
    pub grain_size: u64,
    pub has_compressed_grain: bool,
    pub zeroed_grain_table_entry: bool,
    pub start_sector: u64,
}

#[derive(Debug)]
pub struct FlatStorage {
    pub file: Box<dyn ReadSeek>,
    #[allow(dead_code)]
    pub filename: String,
    pub offset: u64,
    pub start_sector: u64,
}

#[derive(Debug)]
pub enum ExtentStorage {
    Sparse(SparseStorage),
    Flat(FlatStorage),
    Zero,
}

impl ExtentStorage {
    pub fn read(&mut self, offset: u64, buf: &mut [u8]) -> Result<usize, ReadError> {
        match self {
            ExtentStorage::Sparse(storage) => storage.read(offset, buf),
            ExtentStorage::Flat(storage) => storage.read(offset, buf),
            ExtentStorage::Zero => Ok(read_zero(buf)),
        }
    }
}

// We're going off the rails on a crazy grain
#[derive(Debug, thiserror::Error)]
#[error("Sanity check failed for grain index {0}")]
struct CrazyGrainIndex(u64);

#[derive(Debug)]
struct CompressedGrainHeader {
    _lba: u64,
    data_size: u32,
}

fn read_and_decompress_grain(
    file: &mut Box<dyn ReadSeek>,
    grain_index: u64,
    grain_size: u64,
) -> std::io::Result<Vec<u8>> {
    let cgh = CompressedGrainHeader {
        _lba: file.read_u64::<LittleEndian>()?,
        data_size: file.read_u32::<LittleEndian>()?,
    };

    // The decompressed data should not be larger than the grain size.
    // zlib increases the size of incompressible data by a tiny amount
    // so if we see the size of the compressed data is more than twice
    // the grain size, the data size we've read from the header is clearly
    // corrupt.
    if cgh.data_size as u64 > 2 * grain_size {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            CrazyGrainIndex(grain_index),
        ));
    }

    let header: u16 = file.read_u16::<BigEndian>()?;

    // sanity check against expected zlib stream header values...
    if !header.is_multiple_of(31) || header & 0x0F00 != 8 << 8 || header & 0x0020 != 0 {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            CrazyGrainIndex(grain_index),
        ));
    }

    let mut buffer = vec![0u8; cgh.data_size as usize];
    file.read_exact(buffer.as_mut_slice())?;

    let mut decoder = DeflateDecoder::new(&*buffer.as_mut_slice());
    let mut buf = vec![0; grain_size as usize];
    let mut c = 0;

    loop {
        let r = decoder.read(&mut buf[c..])?;
        if r == 0 {
            break;
        }

        if c == buf.len() {
            // The decompressed data is larger than the grain size!
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                CrazyGrainIndex(grain_index),
            ));
        }

        c += r;
    }

    Ok(buf)
}

impl SparseStorage {
    fn read(&mut self, offset: u64, mut buf: &mut [u8]) -> Result<usize, ReadError> {
        // `grain_size` is taken from the image's own sparse header, so a
        // corrupt or crafted image can declare it as zero.
        if self.grain_size == 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("{}: grain size is zero", self.filename),
            )
            .into());
        }

        let grain_size = self.grain_size * SECTOR_SIZE;
        // Rebase the absolute image offset to this extent; the grain table is
        // keyed by grain index within the extent.
        let local = offset - self.start_sector * SECTOR_SIZE;
        let grain_index = local / grain_size;
        let grain_data_offset = (local % grain_size) as usize;

        let r = (grain_size as usize - grain_data_offset).min(buf.len());
        buf = &mut buf[..r];

        // The span map is built from the grain table, so a grain should exist
        // for any offset routed here -- but both come from the image, and a
        // read must not be able to take the process down if they disagree.
        let sector_num = *self.grain_table.get(&grain_index).ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "{}: no grain {grain_index} for offset {offset}",
                    self.filename
                ),
            )
        })?;

        if self.zeroed_grain_table_entry && sector_num == 1 {
            // handle zeroed GTE
            buf.fill(0);
        } else {
            let grain_start = sector_num * SECTOR_SIZE;

            if self.has_compressed_grain {
                self.file.seek(SeekFrom::Start(grain_start))?;

                let grain_data =
                    read_and_decompress_grain(&mut self.file, grain_index, grain_size)?;

                buf.clone_from_slice(&grain_data[grain_data_offset..grain_data_offset + r]);
            } else {
                self.file
                    .seek(SeekFrom::Start(grain_start + grain_data_offset as u64))?;
                self.file.read_exact(buf)?;
            }
        }

        Ok(buf.len())
    }
}

impl FlatStorage {
    fn read(&mut self, offset: u64, buf: &mut [u8]) -> Result<usize, ReadError> {
        // FLAT, VMFS. `offset` is absolute within the image; rebase it to this
        // extent, then add the extent file's own data offset (the descriptor's
        // FLAT offset field, in sectors). Only Flat may have a nonzero field.
        let file_offset = (offset - self.start_sector * SECTOR_SIZE) + self.offset * SECTOR_SIZE;
        self.file.seek(SeekFrom::Start(file_offset))?;
        self.file.read_exact(buf)?;
        Ok(buf.len())
    }
}

fn read_zero(buf: &mut [u8]) -> usize {
    buf.fill(0);
    buf.len()
}

#[cfg(test)]
mod test {
    use super::*;
    use std::io::Cursor;

    fn sparse(grain_size: u64, grain_table: HashMap<u64, u64>) -> SparseStorage {
        SparseStorage {
            file: Box::new(Cursor::new(vec![0u8; 4096])),
            filename: "crafted.vmdk".into(),
            grain_table,
            grain_size,
            has_compressed_grain: false,
            zeroed_grain_table_entry: false,
            start_sector: 0,
        }
    }

    /// `grain_size` comes straight out of the sparse header, so a crafted image
    /// can set it to zero. `local / grain_size` then divides by zero.
    #[test]
    fn zero_grain_size_is_an_error_not_a_divide_by_zero() {
        let mut storage = sparse(0, HashMap::from([(0, 1)]));
        let mut buf = [0u8; 16];

        let err = storage.read(0, &mut buf).unwrap_err();
        assert!(matches!(err, ReadError::IoError(_)), "got {err:?}");
    }

    /// The grain table is built from the image's own metadata; a read whose
    /// grain is missing from it must not take the whole process down.
    #[test]
    fn missing_grain_is_an_error_not_a_panic() {
        let mut storage = sparse(8, HashMap::new());
        let mut buf = [0u8; 16];

        let err = storage.read(0, &mut buf).unwrap_err();
        assert!(matches!(err, ReadError::IoError(_)), "got {err:?}");
    }
}
