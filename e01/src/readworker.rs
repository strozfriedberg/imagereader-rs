use flate2::read::ZlibDecoder;
use simd_adler32::read::adler32;
use std::{
    collections::{HashMap, VecDeque},
    io::{Cursor, Read},
    sync::{Arc, Mutex},
};
use tracing::{debug, error};

use crate::e01_reader::{CorruptChunkPolicy, ReadErrorKind};
use crate::sec_read::Chunk;
use crate::workersource::WorkerSource;

#[derive(Debug)]
pub struct DecodedChunkCache {
    capacity: usize,
    chunks: HashMap<usize, Arc<Vec<u8>>>,
    lru: VecDeque<usize>,
}

impl DecodedChunkCache {
    pub fn new(capacity: usize) -> Self {
        Self {
            capacity,
            chunks: HashMap::with_capacity(capacity),
            lru: VecDeque::with_capacity(capacity),
        }
    }

    pub fn get(&mut self, chunk_index: usize) -> Option<Arc<Vec<u8>>> {
        let chunk = self.chunks.get(&chunk_index)?.clone();
        self.lru.retain(|idx| *idx != chunk_index);
        self.lru.push_back(chunk_index);
        Some(chunk)
    }

    pub fn insert(&mut self, chunk_index: usize, chunk: Arc<Vec<u8>>) {
        if self.capacity == 0 {
            return;
        }

        if self.chunks.insert(chunk_index, chunk).is_some() {
            self.lru.retain(|idx| *idx != chunk_index);
        }
        self.lru.push_back(chunk_index);

        while self.chunks.len() > self.capacity {
            let Some(evicted) = self.lru.pop_front() else {
                break;
            };
            if self.chunks.remove(&evicted).is_none() {
                continue;
            }
        }
    }
}

#[derive(Debug)]
pub struct ReadWorker {
    chunk_size: usize,
    image_end: u64,
    corrupt_chunk_policy: CorruptChunkPolicy,
    decoder: ZlibDecoder<Cursor<Vec<u8>>>,
}

impl Clone for ReadWorker {
    fn clone(&self) -> Self {
        Self::new(self.chunk_size, self.image_end, self.corrupt_chunk_policy)
    }
}

impl ReadWorker {
    pub fn new(
        chunk_size: usize,
        image_end: u64,
        corrupt_chunk_policy: CorruptChunkPolicy,
    ) -> Self {
        Self {
            chunk_size,
            image_end,
            corrupt_chunk_policy,
            decoder: ZlibDecoder::new(Cursor::new(vec![0; chunk_size + 4])),
        }
    }

    fn read_compressed_read<WS: WorkerSource>(
        &mut self,
        src: &mut WS,
        chunk_off: u64,
        chunk_len: usize,
    ) -> Result<(), ReadErrorKind> {
        // take the buffer from the decoder
        let cur = self.decoder.reset(Cursor::new(vec![0; 0]));
        let mut v = cur.into_inner();
        let raw_data = &mut v[..chunk_len];

        // do the read
        let r = src
            .read(chunk_off, raw_data)
            .map_err(ReadErrorKind::IoError);

        // give the buffer back to the decoder
        self.decoder.reset(Cursor::new(v));

        r
    }

    fn read_compressed_decompress_full(
        &mut self,
        chunk_index: usize,
        out: &mut [u8],
    ) -> Result<(), ReadErrorKind> {
        if let Err(e) = self.decoder.read_exact(out) {
            error!("decompression failed for chunk {}: {}", chunk_index, e);
            match self.corrupt_chunk_policy {
                CorruptChunkPolicy::Error => {
                    return Err(ReadErrorKind::DecompressionFailed(chunk_index, e));
                }
                CorruptChunkPolicy::Zero | CorruptChunkPolicy::RawIfPossible => {
                    // zero out corrupt chunk
                    out.fill(0);
                }
            }
        }

        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    fn read_compressed<WS: WorkerSource>(
        &mut self,
        src: &mut WS,
        chunk_index: usize,
        chunk_off: u64,
        chunk_len: usize,
        buf: &mut [u8],
        beg_in_chunk: usize,
        end_in_chunk: usize,
    ) -> Result<(), ReadErrorKind> {
        let chunk_beg = chunk_index as u64 * self.chunk_size as u64;
        let decoded_len = (self.image_end - chunk_beg).min(self.chunk_size as u64) as usize;
        let mut decoded = vec![0; decoded_len];

        self.read_compressed_read(src, chunk_off, chunk_len)?;
        self.read_compressed_decompress_full(chunk_index, &mut decoded)?;

        buf.copy_from_slice(&decoded[beg_in_chunk..end_in_chunk]);

        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    fn read_uncompressed<WS: WorkerSource>(
        &mut self,
        src: &mut WS,
        chunk_index: usize,
        chunk_off: u64,
        chunk_len: usize,
        buf: &mut [u8],
        beg_in_chunk: usize,
        end_in_chunk: usize,
    ) -> Result<(), ReadErrorKind> {
        // take the buffer from the decoder
        let cur = self.decoder.reset(Cursor::new(vec![0; 0]));
        let mut v = cur.into_inner();
        let raw_data = &mut v[..chunk_len];

        // do the read
        self.read_uncompressed_inner(
            src,
            chunk_index,
            chunk_off,
            buf,
            beg_in_chunk,
            end_in_chunk,
            raw_data,
        )?;

        // give the buffer back to the decoder
        self.decoder.reset(Cursor::new(v));

        Ok(())
    }

    fn verify_uncompressed_payload(
        &self,
        chunk_index: usize,
        raw_data: &[u8],
    ) -> Result<Vec<u8>, ReadErrorKind> {
        if raw_data.len() < 5 {
            return Err(ReadErrorKind::TooShort(chunk_index, raw_data.len()));
        }

        let crc_stored = u32::from_le_bytes(
            raw_data[raw_data.len() - 4..]
                .try_into()
                .expect("slice of last 4 bytes not 4 bytes long, wtf"),
        );

        let mut out = raw_data[..raw_data.len() - 4].to_vec();

        let mut reader = Cursor::new(&out);
        let crc = adler32(&mut reader).map_err(ReadErrorKind::IoError)?;

        if crc != crc_stored {
            error!("checksum mismatch reading chunk {}", chunk_index);
            match self.corrupt_chunk_policy {
                CorruptChunkPolicy::Error => {
                    return Err(ReadErrorKind::BadChecksum(chunk_index, crc_stored, crc));
                }
                CorruptChunkPolicy::Zero => {
                    out.fill(0);
                }
                CorruptChunkPolicy::RawIfPossible => {}
            }
        }

        Ok(out)
    }

    #[allow(clippy::too_many_arguments)]
    fn read_uncompressed_inner<WS: WorkerSource>(
        &mut self,
        src: &mut WS,
        chunk_index: usize,
        chunk_off: u64,
        buf: &mut [u8],
        beg_in_chunk: usize,
        end_in_chunk: usize,
        raw_data: &mut [u8],
    ) -> Result<(), ReadErrorKind> {
        src.read(chunk_off, raw_data)
            .map_err(ReadErrorKind::IoError)?;

        let out = self.verify_uncompressed_payload(chunk_index, raw_data)?;
        buf.copy_from_slice(&out[beg_in_chunk..end_in_chunk]);

        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    fn read_uncompressed_cached<WS: WorkerSource>(
        &mut self,
        src: &mut WS,
        chunk_index: usize,
        chunk_off: u64,
        chunk_len: usize,
        buf: &mut [u8],
        beg_in_chunk: usize,
        end_in_chunk: usize,
        cache: &Mutex<DecodedChunkCache>,
    ) -> Result<(), ReadErrorKind> {
        if let Some(chunk) = cache.lock().unwrap().get(chunk_index) {
            buf.copy_from_slice(&chunk[beg_in_chunk..end_in_chunk]);
            return Ok(());
        }

        let mut raw_data = vec![0; chunk_len];
        src.read(chunk_off, &mut raw_data)
            .map_err(ReadErrorKind::IoError)?;

        let payload = self.verify_uncompressed_payload(chunk_index, &raw_data)?;
        buf.copy_from_slice(&payload[beg_in_chunk..end_in_chunk]);
        cache.lock().unwrap().insert(chunk_index, Arc::new(payload));

        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    fn read_compressed_cached<WS: WorkerSource>(
        &mut self,
        src: &mut WS,
        chunk_index: usize,
        chunk_off: u64,
        chunk_len: usize,
        buf: &mut [u8],
        beg_in_chunk: usize,
        end_in_chunk: usize,
        cache: &Mutex<DecodedChunkCache>,
    ) -> Result<(), ReadErrorKind> {
        if let Some(chunk) = cache.lock().unwrap().get(chunk_index) {
            buf.copy_from_slice(&chunk[beg_in_chunk..end_in_chunk]);
            return Ok(());
        }

        let chunk_beg = chunk_index as u64 * self.chunk_size as u64;
        let decoded_len = (self.image_end - chunk_beg).min(self.chunk_size as u64) as usize;
        let mut decoded = vec![0; decoded_len];

        self.read_compressed_read(src, chunk_off, chunk_len)?;
        self.read_compressed_decompress_full(chunk_index, &mut decoded)?;

        buf.copy_from_slice(&decoded[beg_in_chunk..end_in_chunk]);
        cache.lock().unwrap().insert(chunk_index, Arc::new(decoded));

        Ok(())
    }

    pub fn read_cached<WS: WorkerSource>(
        &mut self,
        chunk: &Chunk,
        src: &mut WS,
        chunk_index: usize,
        buf: &mut [u8],
        beg_in_chunk: usize,
        end_in_chunk: usize,
        cache: &Mutex<DecodedChunkCache>,
    ) -> Result<(), ReadErrorKind> {
        let chunk_len = (chunk.end_offset - chunk.data_offset) as usize;
        let chunk_off = chunk.data_offset;

        debug!("reading chunk {chunk_index} [{beg_in_chunk},{end_in_chunk})");

        if chunk.compressed {
            self.read_compressed_cached(
                src,
                chunk_index,
                chunk_off,
                chunk_len,
                buf,
                beg_in_chunk,
                end_in_chunk,
                cache,
            )
        } else {
            self.read_uncompressed_cached(
                src,
                chunk_index,
                chunk_off,
                chunk_len,
                buf,
                beg_in_chunk,
                end_in_chunk,
                cache,
            )
        }
    }

    pub fn read<WS: WorkerSource>(
        &mut self,
        chunk: &Chunk,
        src: &mut WS,
        chunk_index: usize,
        buf: &mut [u8],
        beg_in_chunk: usize,
        end_in_chunk: usize,
    ) -> Result<(), ReadErrorKind> {
        let chunk_len = (chunk.end_offset - chunk.data_offset) as usize;
        let chunk_off = chunk.data_offset;

        debug!("reading chunk {chunk_index} [{beg_in_chunk},{end_in_chunk})");

        if chunk.compressed {
            self.read_compressed(
                src,
                chunk_index,
                chunk_off,
                chunk_len,
                buf,
                beg_in_chunk,
                end_in_chunk,
            )
        } else {
            self.read_uncompressed(
                src,
                chunk_index,
                chunk_off,
                chunk_len,
                buf,
                beg_in_chunk,
                end_in_chunk,
            )
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workersource::WorkerSource;
    use flate2::{Compression, write::ZlibEncoder};
    use std::io::Write;

    struct VecSource {
        data: Vec<u8>,
        read_count: usize,
    }

    impl WorkerSource for VecSource {
        fn read(&mut self, off: u64, buf: &mut [u8]) -> Result<(), std::io::Error> {
            self.read_count += 1;
            let off = off as usize;
            buf.copy_from_slice(&self.data[off..off + buf.len()]);
            Ok(())
        }
    }

    fn compressed_chunk(data: &[u8]) -> Vec<u8> {
        let mut encoder = ZlibEncoder::new(Vec::new(), Compression::default());
        encoder.write_all(data).unwrap();
        encoder.finish().unwrap()
    }

    #[test]
    fn compressed_partial_read_returns_requested_offset() {
        let chunk_size = 32768;
        let data = (0..chunk_size)
            .map(|i| ((i / 251) ^ i) as u8)
            .collect::<Vec<_>>();
        let compressed = compressed_chunk(&data);
        let chunk = Chunk {
            segment: 0,
            data_offset: 0,
            end_offset: compressed.len() as u64,
            compressed: true,
        };
        let mut src = VecSource {
            data: compressed,
            read_count: 0,
        };
        let mut worker = ReadWorker::new(chunk_size, chunk_size as u64, CorruptChunkPolicy::Error);
        let mut out = vec![0; 4096];

        worker
            .read(&chunk, &mut src, 0, &mut out, 4096, 8192)
            .unwrap();

        assert_eq!(&out, &data[4096..8192]);
    }

    fn uncompressed_chunk_with_trailer(data: &[u8]) -> Vec<u8> {
        let mut raw = data.to_vec();
        let crc = adler32(&mut Cursor::new(data)).unwrap();
        raw.extend_from_slice(&crc.to_le_bytes());
        raw
    }

    #[test]
    fn uncompressed_partial_read_reuses_decoded_chunk_cache() {
        let chunk_size = 32768;
        let data = (0..chunk_size)
            .map(|i| ((i / 251) ^ i) as u8)
            .collect::<Vec<_>>();
        let raw = uncompressed_chunk_with_trailer(&data);
        let chunk = Chunk {
            segment: 0,
            data_offset: 0,
            end_offset: raw.len() as u64,
            compressed: false,
        };
        let mut src = VecSource {
            data: raw,
            read_count: 0,
        };
        let mut worker = ReadWorker::new(chunk_size, chunk_size as u64, CorruptChunkPolicy::Error);
        let cache = Mutex::new(DecodedChunkCache::new(8));

        let mut first = vec![0; 4096];
        worker
            .read_cached(&chunk, &mut src, 0, &mut first, 0, 4096, &cache)
            .unwrap();
        assert_eq!(&first, &data[0..4096]);
        assert_eq!(src.read_count, 1);

        let mut second = vec![0; 4096];
        worker
            .read_cached(&chunk, &mut src, 0, &mut second, 4096, 8192, &cache)
            .unwrap();
        assert_eq!(&second, &data[4096..8192]);
        assert_eq!(src.read_count, 1);
    }

    #[test]
    fn compressed_partial_read_reuses_decoded_chunk_cache() {
        let chunk_size = 32768;
        let data = (0..chunk_size)
            .map(|i| ((i / 251) ^ i) as u8)
            .collect::<Vec<_>>();
        let compressed = compressed_chunk(&data);
        let chunk = Chunk {
            segment: 0,
            data_offset: 0,
            end_offset: compressed.len() as u64,
            compressed: true,
        };
        let mut src = VecSource {
            data: compressed,
            read_count: 0,
        };
        let mut worker = ReadWorker::new(chunk_size, chunk_size as u64, CorruptChunkPolicy::Error);
        let cache = Mutex::new(DecodedChunkCache::new(8));

        let mut first = vec![0; 4096];
        worker
            .read_cached(&chunk, &mut src, 0, &mut first, 0, 4096, &cache)
            .unwrap();
        assert_eq!(&first, &data[0..4096]);
        assert_eq!(src.read_count, 1);

        let mut second = vec![0; 4096];
        worker
            .read_cached(&chunk, &mut src, 0, &mut second, 4096, 8192, &cache)
            .unwrap();
        assert_eq!(&second, &data[4096..8192]);
        assert_eq!(src.read_count, 1);
    }
}
