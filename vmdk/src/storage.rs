use byteorder::{BigEndian, LittleEndian, ReadBytesExt};
use flate2::read::DeflateDecoder;
use std::{
    collections::HashMap,
    fmt::Debug,
    io::{self, Read, SeekFrom},
};

use crate::vmdk_reader::ReadError;
use imagesource::ReadSeek;

const SECTOR_SIZE: u64 = 512;

/// Something that can hand out a fresh cursor over an extent's bytes.
///
/// The storages used to hold one `Box<dyn ReadSeek>` and seek it, which meant
/// `read` needed `&mut self` -- and that `&mut` propagated all the way up to
/// `VmdkReader::read_at_offset`, forcing a server to put the whole reader behind
/// one lock. A file position cannot be shared between threads; a cursor per read
/// can. Minting one is cheap (a `CacheReadSeek` is three `Arc` clones and an
/// offset -- no I/O).
pub trait ReadSeekSource: Debug + Send + Sync {
    fn cursor(&self) -> Box<dyn ReadSeek>;
}

/// Anything cloneable that can be read and seeked can mint cursors: cloning a
/// `CacheReadSeek` is three `Arc` clones and an offset.
impl<R> ReadSeekSource for R
where
    R: ReadSeek + Clone + Debug + Sync + 'static,
{
    fn cursor(&self) -> Box<dyn ReadSeek> {
        Box::new(self.clone())
    }
}

#[derive(Debug)]
pub struct SparseStorage {
    pub source: Box<dyn ReadSeekSource>,
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
    pub source: Box<dyn ReadSeekSource>,
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
    pub fn read(&self, offset: u64, buf: &mut [u8]) -> Result<usize, ReadError> {
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
    file: &mut dyn ReadSeek,
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
        if c == buf.len() {
            // The buffer is full. If the decoder still has output waiting, the
            // grain decompresses to more than grain_size and is corrupt. (The
            // old check sat after the `r == 0` break and never ran: reading
            // into the now-empty `buf[c..]` returns Ok(0) first, silently
            // truncating an over-long grain instead of reporting it.)
            let mut overflow = [0u8; 1];
            if decoder.read(&mut overflow)? != 0 {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    CrazyGrainIndex(grain_index),
                ));
            }
            break;
        }

        let r = decoder.read(&mut buf[c..])?;
        if r == 0 {
            break;
        }

        c += r;
    }

    Ok(buf)
}

impl SparseStorage {
    fn read(&self, offset: u64, mut buf: &mut [u8]) -> Result<usize, ReadError> {
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

            // A cursor of our own, so concurrent reads cannot move each other's
            // file position.
            let mut file = self.source.cursor();

            if self.has_compressed_grain {
                file.seek(SeekFrom::Start(grain_start))?;

                let grain_data = read_and_decompress_grain(&mut *file, grain_index, grain_size)?;

                buf.clone_from_slice(&grain_data[grain_data_offset..grain_data_offset + r]);
            } else {
                file.seek(SeekFrom::Start(grain_start + grain_data_offset as u64))?;
                file.read_exact(buf)?;
            }
        }

        Ok(buf.len())
    }
}

impl FlatStorage {
    fn read(&self, offset: u64, buf: &mut [u8]) -> Result<usize, ReadError> {
        // FLAT, VMFS. `offset` is absolute within the image; rebase it to this
        // extent, then add the extent file's own data offset (the descriptor's
        // FLAT offset field, in sectors). Only Flat may have a nonzero field.
        let file_offset = (offset - self.start_sector * SECTOR_SIZE) + self.offset * SECTOR_SIZE;

        let mut file = self.source.cursor();
        file.seek(SeekFrom::Start(file_offset))?;
        file.read_exact(buf)?;
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
    use flate2::{Compression, write::DeflateEncoder};
    use std::io::{Cursor, Write};

    /// Build the on-disk bytes for one compressed grain: the 12-byte header
    /// (lba + data_size), a valid 2-byte zlib header, then the raw deflate
    /// stream of `payload`.
    fn compressed_grain(payload: &[u8]) -> Vec<u8> {
        let mut enc = DeflateEncoder::new(Vec::new(), Compression::default());
        enc.write_all(payload).unwrap();
        let deflate = enc.finish().unwrap();

        let mut bytes = Vec::new();
        bytes.extend_from_slice(&0u64.to_le_bytes()); // lba
        bytes.extend_from_slice(&(deflate.len() as u32).to_le_bytes()); // data_size
        bytes.extend_from_slice(&[0x78, 0x9c]); // zlib header; raw deflate follows
        bytes.extend_from_slice(&deflate);
        bytes
    }

    /// A grain that decompresses to more than grain_size is corrupt. The old
    /// over-long check was unreachable, so such a grain was silently truncated;
    /// it must now surface as an InvalidData error.
    #[test]
    fn over_long_grain_is_reported_not_truncated() {
        let grain_size = 16;
        // 64 bytes of highly compressible data: decompresses to 4x grain_size
        // while the compressed data_size stays under the 2*grain_size gate.
        let bytes = compressed_grain(&[0xABu8; 64]);
        let mut file = Cursor::new(bytes);

        let err = read_and_decompress_grain(&mut file, 0, grain_size).unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::InvalidData);
    }

    /// A grain that decompresses to exactly grain_size is valid and returned
    /// intact.
    #[test]
    fn exact_size_grain_decompresses() {
        let grain_size = 64;
        let payload = vec![0xCDu8; grain_size as usize];
        let bytes = compressed_grain(&payload);
        let mut file = Cursor::new(bytes);

        let out = read_and_decompress_grain(&mut file, 0, grain_size).unwrap();
        assert_eq!(out, payload);
    }

    fn sparse(grain_size: u64, grain_table: HashMap<u64, u64>) -> SparseStorage {
        SparseStorage {
            source: Box::new(Cursor::new(vec![0u8; 4096])),
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
