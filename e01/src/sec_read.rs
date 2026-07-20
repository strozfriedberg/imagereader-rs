use crate::error::{IoError, LibError, MAX_TABLE_ENTRIES};
use crate::generated::{
    ewf_digest_section::EwfDigestSection, ewf_hash_section::EwfHashSection,
    ewf_section_descriptor_v1::EwfSectionDescriptorV1, ewf_table_header::EwfTableHeader,
    ewf_volume::EwfVolume, ewf_volume_smart::EwfVolumeSmart,
};
//use crate::generated::ewf_section_descriptor_v2::*;

use kaitai::{BytesReader, KStream, KStruct};

#[derive(Debug)]
pub struct Chunk {
    pub segment: usize,
    pub data_offset: u64,
    pub end_offset: u64,
    pub compressed: bool,
}

#[derive(Debug)]
pub enum Section {
    Volume(VolumeSection),
    Table(Vec<Chunk>),
    Sectors(u64),
    Hash([u8; 16]),
    Digest([u8; 16], [u8; 20]),
    Done,
    Other,
}

fn checksum_reader(reader: &BytesReader, len: usize) -> Result<u32, IoError> {
    Ok(adler32::adler32(std::io::Cursor::new(
        &reader.read_bytes(len).map_err(IoError::Read)?,
    ))?)
}

fn checksum_ok(
    section_type: &str,
    io: &BytesReader,
    section_io: &BytesReader,
    crc_stored: u32,
) -> Result<(), LibError> {
    let crc = checksum_reader(section_io, io.pos() - section_io.pos() - 4)?;
    match crc == crc_stored {
        true => Ok(()),
        false => Err(LibError::BadChecksum(section_type.into(), crc, crc_stored)),
    }
}

fn read_section(io: &BytesReader, ignore_checksums: bool) -> Result<(usize, Section), LibError> {
    let sd = EwfSectionDescriptorV1::read_into::<_, EwfSectionDescriptorV1>(io, None, None)
        .map_err(|e| LibError::DeserializationFailed("EwfFileHeaderV1", e))?;

    let section_size = if *sd.size() > 0x4c {
        // header size
        *sd.size() - 0x4c
    } else {
        0
    };

    let section_type_full = sd.type_string();
    let section_type = section_type_full.trim_matches(char::from(0));

    let section = match section_type {
        "disk" | "volume" => {
            Section::Volume(VolumeSection::new(io, section_size, ignore_checksums)?)
        }
        "table" => Section::Table(read_table(io, section_size, ignore_checksums)?),
        "sectors" => Section::Sectors(io.pos() as u64 + section_size),
        "hash" => Section::Hash(read_hash_section(io, ignore_checksums)?),
        "digest" => {
            let (md5, sha1) = read_digest_section(io, ignore_checksums)?;
            Section::Digest(md5, sha1)
        }
        "done" => Section::Done,
        _ => Section::Other,
    };

    let section_offset = *sd.next_offset() as usize;

    Ok((section_offset, section))
}

fn read_hash_section(io: &BytesReader, ignore_checksums: bool) -> Result<[u8; 16], LibError> {
    let hash_section = EwfHashSection::read_into::<_, EwfHashSection>(io, None, None)
        .map_err(|e| LibError::DeserializationFailed("EwfHashSection", e))?;

    if !ignore_checksums {
        checksum_ok(
            "Hash section",
            io,
            &hash_section._io(),
            *hash_section.checksum(),
        )?;
    }

    let md5 = hash_section
        .md5()
        .as_slice()
        .try_into()
        .expect("MD5 must deserialize to 16 bytes");

    Ok(md5)
}

fn read_digest_section(
    io: &BytesReader,
    ignore_checksums: bool,
) -> Result<([u8; 16], [u8; 20]), LibError> {
    let digest_section = EwfHashSection::read_into::<_, EwfDigestSection>(io, None, None)
        .map_err(|e| LibError::DeserializationFailed("EwfDigestSection", e))?;

    if !ignore_checksums {
        checksum_ok(
            "Digest section",
            io,
            &digest_section._io(),
            *digest_section.checksum(),
        )?;
    }

    let md5 = digest_section
        .md5()
        .as_slice()
        .try_into()
        .expect("MD5 must deserialize to 16 bytes");

    let sha1 = digest_section
        .sha1()
        .as_slice()
        .try_into()
        .expect("SHA1 must deserialize to 20 bytes");

    Ok((md5, sha1))
}

fn read_table_entry(io: &BytesReader, table_offset: u64) -> Result<Chunk, LibError> {
    let entry = io.read_u4le().map_err(IoError::Read)?;

    Ok(Chunk {
        segment: 0,
        data_offset: table_offset + ((entry & 0x7fffffff) as u64),
        end_offset: 0,
        compressed: (entry & 0x80000000) > 0,
    })
}

pub fn read_table(
    io: &BytesReader,
    _size: u64,
    ignore_checksums: bool,
) -> Result<Vec<Chunk>, LibError> {
    let table_section = EwfTableHeader::read_into::<_, EwfTableHeader>(io, None, None)
        .map_err(|e| LibError::DeserializationFailed("EwfTableHeader", e))?;

    if !ignore_checksums {
        checksum_ok(
            "Table section",
            io,
            &table_section._io(),
            *table_section.checksum(),
        )?;
    }

    let raw_entry_count = *table_section.entry_count();
    if raw_entry_count > MAX_TABLE_ENTRIES {
        return Err(LibError::TooManyTableEntries(raw_entry_count));
    }

    let entry_count = raw_entry_count as usize;
    if entry_count == 0 {
        // weird, but possible?
        return Ok(vec![]);
    }

    let io_offsets = Clone::clone(io);
    let table_offset = *table_section.table_base_offset();
    let mut chunks: Vec<Chunk> = Vec::with_capacity(entry_count);

    chunks.push(read_table_entry(io, table_offset)?);

    for i in 1..entry_count {
        let ch = read_table_entry(io, table_offset)?;
        // each entry is the previous chunk's end; going backwards would give
        // that chunk a negative length
        if ch.data_offset < chunks[i - 1].data_offset {
            return Err(LibError::CorruptChunkTable(format!(
                "entry {} offset {} precedes entry {} offset {}",
                i,
                ch.data_offset,
                i - 1,
                chunks[i - 1].data_offset,
            )));
        }
        chunks[i - 1].end_offset = ch.data_offset;
        chunks.push(ch);
    }

    if !ignore_checksums {
        // table footer
        let crc_stored = io.read_u4le().map_err(IoError::Read)?;

        let crc = checksum_reader(&io_offsets, *table_section.entry_count() as usize * 4)?;

        if crc != crc_stored {
            return Err(LibError::BadChecksum(
                "Table offset array".into(),
                crc,
                crc_stored,
            ));
        }
    }

    Ok(chunks)
}

#[derive(Debug, Default)]
pub struct VolumeSection {
    pub chunk_count: u32,
    pub sectors_per_chunk: u32,
    pub bytes_per_sector: u32,
    pub total_sector_count: u64,
}

impl VolumeSection {
    pub fn new(io: &BytesReader, size: u64, ignore_checksums: bool) -> Result<Self, LibError> {
        // read volume section
        if size == 1052 {
            let vol_section = EwfVolume::read_into::<_, EwfVolume>(io, None, None)
                .map_err(|e| LibError::DeserializationFailed("EwfVolume", e))?;

            if !ignore_checksums {
                checksum_ok(
                    "Volume section",
                    io,
                    &vol_section._io(),
                    *vol_section.checksum(),
                )?;
            }

            let vs = VolumeSection {
                chunk_count: *vol_section.number_of_chunks(),
                sectors_per_chunk: *vol_section.sectors_per_chunk(),
                bytes_per_sector: *vol_section.bytes_per_sector(),
                total_sector_count: *vol_section.number_of_sectors(),
            };
            Ok(vs)
        } else if size == 94 {
            let vol_section = EwfVolumeSmart::read_into::<_, EwfVolumeSmart>(io, None, None)
                .map_err(|e| LibError::DeserializationFailed("EwfVolumeSmart", e))?;

            if !ignore_checksums {
                checksum_ok(
                    "Volume section",
                    io,
                    &vol_section._io(),
                    *vol_section.checksum(),
                )?;
            }

            let vs = VolumeSection {
                chunk_count: *vol_section.number_of_chunks(),
                sectors_per_chunk: *vol_section.sectors_per_chunk(),
                bytes_per_sector: *vol_section.bytes_per_sector(),
                total_sector_count: *vol_section.number_of_sectors() as u64,
            };
            Ok(vs)
        } else {
            Err(LibError::UnexpectedVolumeSize(size))
        }
    }

    pub fn chunk_size(&self) -> usize {
        self.sectors_per_chunk as usize * self.bytes_per_sector as usize
    }

    pub fn max_offset(&self) -> usize {
        self.total_sector_count as usize * self.bytes_per_sector as usize
    }
}

pub struct SectionIterator<'a> {
    io: &'a BytesReader,
    current_offset: usize,
    ignore_checksums: bool,
}

impl<'a> SectionIterator<'a> {
    pub fn new(io: &'a BytesReader, ignore_checksums: bool) -> Self {
        Self {
            io,
            current_offset: io.pos(),
            ignore_checksums,
        }
    }
}

impl Iterator for SectionIterator<'_> {
    type Item = Result<Section, LibError>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.current_offset < self.io.size() {
            if let Err(e) = self.io.seek(self.current_offset) {
                return Some(Err(LibError::IoError(IoError::Seek(
                    self.current_offset,
                    e,
                ))));
            }

            match read_section(self.io, self.ignore_checksums) {
                Ok((section_offset, section)) => {
                    // Sections advance forward through the file; the final
                    // section's next_offset points at itself. A next_offset
                    // that does not move forward — a self-pointer or a
                    // backward cycle from a corrupt image — ends iteration.
                    // Requiring strict forward progress prevents a two-section
                    // A->B->A cycle from looping forever.
                    self.current_offset = if section_offset > self.current_offset {
                        section_offset
                    } else {
                        // ensure that the next() next is None
                        self.io.size()
                    };

                    Some(Ok(section))
                }
                Err(e) => Some(Err(e)),
            }
        } else {
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// table header (24 bytes) followed by 4-byte offset entries
    fn table_bytes(entry_count: u32, entries: &[u32]) -> Vec<u8> {
        let mut b = vec![];
        b.extend_from_slice(&entry_count.to_le_bytes());
        b.extend_from_slice(&[0u8; 4]); // padding1
        b.extend_from_slice(&0u64.to_le_bytes()); // table_base_offset
        b.extend_from_slice(&[0u8; 4]); // padding2
        b.extend_from_slice(&0u32.to_le_bytes()); // checksum
        for e in entries {
            b.extend_from_slice(&e.to_le_bytes());
        }
        b
    }

    /// Chunk data offsets must be monotonic — each entry is the previous
    /// chunk's end. A table where they go backwards would give a chunk a
    /// negative length downstream.
    #[test]
    fn non_monotonic_table_entries_are_an_error() {
        let io = BytesReader::from(table_bytes(2, &[0x100, 0x80]));
        let err = read_table(&io, 0, true).unwrap_err();
        assert!(matches!(err, LibError::CorruptChunkTable(_)), "got {err:?}");
    }

    /// entry_count is a raw u32 from the image; EWF caps table entries at
    /// 65534. An absurd count must be rejected before it drives a
    /// multi-gigabyte allocation.
    #[test]
    fn huge_table_entry_count_is_rejected_before_allocation() {
        let io = BytesReader::from(table_bytes(u32::MAX, &[]));
        let err = read_table(&io, 0, true).unwrap_err();
        assert!(matches!(err, LibError::TooManyTableEntries(_)), "got {err:?}");
    }

    /// A section descriptor v1 record: 16-byte type, u64 next_offset, u64
    /// size, 40 bytes padding, u32 checksum (76 bytes total). An unknown
    /// type parses as Section::Other with no checksum validation.
    fn section_desc(type_str: &str, next_offset: u64, size: u64) -> Vec<u8> {
        let mut b = vec![0u8; 76];
        let t = type_str.as_bytes();
        let n = t.len().min(16);
        b[..n].copy_from_slice(&t[..n]);
        b[16..24].copy_from_slice(&next_offset.to_le_bytes());
        b[24..32].copy_from_slice(&size.to_le_bytes());
        b
    }

    /// next_offset comes from the image. Valid sections advance forward and
    /// the terminator points at itself; a corrupt image can point section B
    /// back at section A. That must terminate iteration, not hang forever.
    #[test]
    fn cyclic_section_offsets_terminate_instead_of_looping() {
        let mut bytes = section_desc("junk", 76, 76); // section at 0 -> 76
        bytes.extend(section_desc("junk", 0, 76)); // section at 76 -> 0
        let io = BytesReader::from(bytes);

        // Without forward-progress enforcement this never returns.
        let sections: Vec<_> = SectionIterator::new(&io, true).collect();
        assert_eq!(sections.len(), 2);
        assert!(sections.iter().all(|s| s.is_ok()), "got {sections:?}");
    }
}
