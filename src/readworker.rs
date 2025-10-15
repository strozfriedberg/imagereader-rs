use flate2::read::ZlibDecoder;
use simd_adler32::read::adler32;
use std::io::{Cursor, Read};
use tracing::error;

use crate::bytessource::BytesSource;
use crate::e01_reader::{CorruptChunkPolicy, ReadErrorKind};
use crate::sec_read::Chunk;

#[derive(Debug)]
pub struct ReadWorker {
    chunk_size: usize,
    image_end: usize,
    corrupt_chunk_policy: CorruptChunkPolicy,
    scratch: Vec<u8>,
    decoder: ZlibDecoder<Cursor<Vec<u8>>>
}

impl Clone for ReadWorker {
    fn clone(&self) -> Self {
        Self::new(
            self.chunk_size,
            self.image_end,
            self.corrupt_chunk_policy
        )
    }
}

impl ReadWorker {
    pub fn new(
        chunk_size: usize,
        image_end: usize,
        corrupt_chunk_policy: CorruptChunkPolicy
    ) -> Self
    {
        Self {
            chunk_size,
            image_end,
            corrupt_chunk_policy,
            scratch: vec![0; chunk_size],
            decoder: ZlibDecoder::new(Cursor::new(vec![0; chunk_size + 4]))
        }
    }

    fn read_compressed_read<BS: BytesSource>(
        &mut self,
        handle: &mut BS,
        chunk_off: u64,
        chunk_len: usize
    ) -> Result<(), ReadErrorKind>
    {
        // take the buffer from the decoder
        let cur = self.decoder.reset(Cursor::new(vec![0; 0]));
        let mut v = cur.into_inner();
        let raw_data = &mut v[..chunk_len];

        // do the read
        let r = handle.read(chunk_off, raw_data)
            .map_err(ReadErrorKind::IoError);

        // give the buffer back to the decoder
        self.decoder.reset(Cursor::new(v));

        r
    }

    fn read_compressed_decompress(
        &mut self,
        chunk_index: usize,
        chunk_len: usize,
        buf: &mut [u8],
        beg_in_chunk: usize,
        end_in_chunk: usize
    ) -> Result<usize, ReadErrorKind>
    {
        // Every chunk contains the same amount of data except for the last
        // one; decompress directly into the buffer if there is sufficient
        // space.

        let (mut out, use_scratch) = if buf.len() == self.chunk_size ||
            (buf.len() < self.chunk_size &&
            chunk_index * self.chunk_size > self.image_end)
        {
            // decompress directly into output buffer
            (&mut buf[..], false)
        }
        else {
            // decompress into scratch buffer
            (&mut self.scratch[..], true)
        };

        // compressed chunks are either ok or unrecoverable
        if let Err(e) = self.decoder.read_exact(&mut out) {
            error!("decompression failed for chunk {}: {}", chunk_index, e);
            match self.corrupt_chunk_policy {
                CorruptChunkPolicy::Error => return Err(
                    ReadErrorKind::DecompressionFailed(chunk_index, e)
                ),
                CorruptChunkPolicy::Zero |
                CorruptChunkPolicy::RawIfPossible => {
                    // zero out corrupt chunk
                    out.fill(0);
                }
            }
        }

        // copy requested portion of scratch into user buffer
        if use_scratch {
            let out = &self.scratch[..];
            buf.copy_from_slice(&out[beg_in_chunk..end_in_chunk]);
        }

        Ok(buf.len())
    }

    fn read_compressed<BS: BytesSource>(
        &mut self,
        handle: &mut BS,
        chunk_index: usize,
        chunk_off: u64,
        chunk_len: usize,
        buf: &mut [u8],
        beg_in_chunk: usize,
        end_in_chunk: usize
    ) -> Result<usize, ReadErrorKind>
    {
        self.read_compressed_read(handle, chunk_off, chunk_len)?;
        self.read_compressed_decompress(
            chunk_index,
            chunk_len,
            buf,
            beg_in_chunk,
            end_in_chunk
        )
    }

    fn read_uncompressed<BS: BytesSource>(
        &mut self,
        handle: &mut BS,
        chunk_index: usize,
        chunk_off: u64,
        chunk_len: usize,
        buf: &mut [u8],
        beg_in_chunk: usize,
        end_in_chunk: usize
    ) -> Result<usize, ReadErrorKind>
    {
        // take the buffer from the decoder
        let cur = self.decoder.reset(Cursor::new(vec![0; 0]));
        let mut v = cur.into_inner();
        let raw_data = &mut v[..chunk_len];

        // do the read
        let r = self.read_uncompressed_inner(
            handle,
            chunk_index,
            chunk_off,
            buf,
            beg_in_chunk,
            end_in_chunk,
            raw_data
        );

        // give the buffer back to the decoder
        self.decoder.reset(Cursor::new(v));

        r
    }

    fn read_uncompressed_inner<BS: BytesSource>(
        &mut self,
        handle: &mut BS,
        chunk_index: usize,
        chunk_off: u64,
        buf: &mut [u8],
        beg_in_chunk: usize,
        end_in_chunk: usize,
        raw_data: &mut [u8]
    ) -> Result<usize, ReadErrorKind>
    {
        handle.read(chunk_off, raw_data)
            .map_err(ReadErrorKind::IoError)?;

        let raw_data_len = raw_data.len();
        if raw_data_len < 5 {
            return Err(ReadErrorKind::TooShort(chunk_index, raw_data_len));
        }

        // read stored checksum
        let crc_stored = u32::from_le_bytes(
            raw_data[raw_data_len - 4..]
                .try_into()
                .expect("slice of last 4 bytes not 4 bytes long, wtf")
        );

        // trim stored checksum from data
        let out = &mut raw_data[..raw_data_len - 4];

        // checksum the data
        let mut reader = Cursor::new(&out);
        let crc = adler32(&mut reader)
            .map_err(ReadErrorKind::IoError)?;

        // deal with checksum mismatch
        if crc != crc_stored {
            error!("checksum mismatch reading chunk {}", chunk_index);
            match self.corrupt_chunk_policy {
                CorruptChunkPolicy::Error => return Err(
                    ReadErrorKind::BadChecksum(chunk_index, crc_stored, crc)
                ),
                CorruptChunkPolicy::Zero => {
                    // zero out corrupt chunk
                    out.fill(0);
                },
                CorruptChunkPolicy::RawIfPossible => {
                    // let's gooooooooo!
                }
            }
        }

        buf.copy_from_slice(&out[beg_in_chunk..end_in_chunk]);

        Ok(buf.len())
    }

    pub fn read<BS: BytesSource>(
        &mut self,
        chunk: &Chunk,
        handle: &mut BS,
        chunk_index: usize,
        buf: &mut [u8],
        beg_in_chunk: usize,
        end_in_chunk: usize
    ) -> Result<usize, ReadErrorKind>
    {
        let chunk_len = (chunk.end_offset - chunk.data_offset) as usize;
        let chunk_off = chunk.data_offset;

        // read the data into the buffer
        if chunk.compressed {
            self.read_compressed(
                handle,
                chunk_index,
                chunk_off,
                chunk_len,
                buf,
                beg_in_chunk,
                end_in_chunk
            )
        }
        else {
            self.read_uncompressed(
                handle,
                chunk_index,
                chunk_off,
                chunk_len,
                buf,
                beg_in_chunk,
                end_in_chunk
            )
        }
    }
}
