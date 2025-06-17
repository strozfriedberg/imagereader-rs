use crate::checksum::checksum;
use crate::e01_reader::{E01Error, FuckOffKError, SegmentFileHeader};
use crate::sec_read::{self, Chunk, Section, VolumeSection};

use flate2::read::ZlibDecoder;
use kaitai::{BytesReader, KStream};
use std::{
    io::Read,
    path::Path
};


#[derive(Debug)]
pub struct Segment {
    io: BytesReader,
    _header: SegmentFileHeader,
    chunks: Vec<Chunk>,
    end_of_sectors: u64,
}

impl Segment {
    pub fn read<T: AsRef<Path>>(
        f: T,
        volume: &mut Option<VolumeSection>,
        stored_md5: &mut Option<Vec<u8>>,
        stored_sha1: &mut Option<Vec<u8>>,
        ignore_checksums: bool,
    ) -> Result<Self, E01Error> {
        let io = BytesReader::open(f.as_ref())
            .map_err(|e| E01Error::OpenError { source: FuckOffKError(e) })?;
        let header = SegmentFileHeader::new(&io)?;
        let mut chunks: Vec<Chunk> = Vec::new();
        let mut end_of_sectors = 0;
        let mut current_offset = io.pos();
        while current_offset < io.size() {
            io.seek(current_offset).map_err(|e|
                E01Error::SegmentSeekError {
                    file: f.as_ref().into(),
                    offset: current_offset,
                    source: FuckOffKError(e)
                }
            )?;

            let (section_offset, section) = sec_read::read_section(
                &io, ignore_checksums
            )?;

            match section {
                Section::Volume(v) => *volume = Some(v),
                Section::Table(t) => {
                    chunks.extend(t);
                    let chunks_len = chunks.len();
                    chunks[chunks_len - 1].end_offset = Some(end_of_sectors);
                },
                Section::Sectors(eos) => end_of_sectors = eos,
                Section::Hash(h) => *stored_md5 = Some(h.md5().clone()),
                Section::Digest(d) => {
                    *stored_md5 = Some(d.md5().clone());
                    *stored_sha1 = Some(d.sha1().clone());
                },
                Section::Done => break,
                _ => {}
            }

            if current_offset == section_offset {
                break;
            }

            current_offset = section_offset;
        }

        let segment = Segment {
            io,
            _header: header,
            chunks,
            end_of_sectors,
        };

        Ok(segment)
    }

    pub fn read_chunk(
        &self,
        chunk_number: usize,
        chunk_index: usize,
        ignore_checksums: bool,
        buf: &mut [u8]
    ) -> Result<Vec<u8>, E01Error>
    {
        debug_assert!(chunk_index < self.chunks.len());
        let chunk = &self.chunks[chunk_index];
        self.io
            .seek(chunk.data_offset as usize)
            .map_err(|e| E01Error::SeekError { source: FuckOffKError(e) })?;

        let end_offset = if chunk_index == self.chunks.len() - 1 {
            self.end_of_sectors
        }
        else if let Some(end_of_section) = chunk.end_offset {
            end_of_section
        }
        else {
            self.chunks[chunk_index + 1].data_offset
        };

        let mut raw_data = self
            .io
            .read_bytes(end_offset as usize - chunk.data_offset as usize)
            .map_err(|e| E01Error::ReadError { source: FuckOffKError(e) })?;

        if !chunk.compressed {
            if !ignore_checksums {
                // read stored checksum
                let crc_stored = u32::from_le_bytes(
                    raw_data[raw_data.len() - 4..]
                        .try_into()
                        .expect("slice of last 4 bytes not 4 bytes long, wtf")
                );

                // remove stored checksum from data
                raw_data.truncate(raw_data.len() - 4);

                // checksum the data
                let crc = checksum(&raw_data)?;

                if crc != crc_stored {
                    return Err(E01Error::BadChecksum(
                        format!("Chunk {}", chunk_number),
                        crc,
                        crc_stored
                    ));
                }
            }

            Ok(raw_data)
        }
        else {
            let mut decoder = ZlibDecoder::new(&raw_data[..]);
            let mut data = Vec::new();
            decoder
                .read_to_end(&mut data)
                .map_err(E01Error::DecompressionFailed)?;
            Ok(data)
        }
    }

    pub fn chunk_count(&self) -> usize {
        self.chunks.len()
    }
}
