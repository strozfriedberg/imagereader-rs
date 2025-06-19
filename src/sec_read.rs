use crate::error::{IoError, LibError};
use crate::generated::{
    ewf_digest_section::EwfDigestSection,
    ewf_hash_section::EwfHashSection,
    ewf_section_descriptor_v1::EwfSectionDescriptorV1,
    ewf_table_header::EwfTableHeader,
    ewf_volume::EwfVolume,
    ewf_volume_smart::EwfVolumeSmart
};
//use crate::generated::ewf_section_descriptor_v2::*;

use kaitai::{BytesReader, KStream, KStruct, OptRc};

#[derive(Debug)]
pub struct Chunk {
    pub data_offset: u64,
    pub compressed: bool,
    pub end_offset: Option<u64>
}

#[derive(Debug)]
pub enum Section {
    Volume(VolumeSection),
    Table(Vec<Chunk>),
    Sectors(u64),
    Hash(OptRc<EwfHashSection>),
    Digest(OptRc<EwfDigestSection>),
    Done,
    Other
}

fn checksum_reader(
    reader: &BytesReader,
    len: usize
) -> Result<u32, IoError>
{
    Ok(adler32::adler32(std::io::Cursor::new(
        &reader
            .read_bytes(len)
            .map_err(IoError::ReadError)?
    ))?)
}

fn checksum_ok(
    section_type: &str,
    io: &BytesReader,
    section_io: &BytesReader,
    crc_stored: u32,
) -> Result<(), LibError>
{
    let crc = checksum_reader(section_io, io.pos() - section_io.pos() - 4)?;
    match crc == crc_stored {
        true => Ok(()),
        false => Err(LibError::BadChecksum(section_type.into(), crc, crc_stored))
    }
}

fn read_section(
    io: &BytesReader,
    ignore_checksums: bool
) -> Result<(usize, Section), LibError> {

    let sd = EwfSectionDescriptorV1::read_into::<_, EwfSectionDescriptorV1>(io, None, None)
        .map_err(|e| LibError::DeserializationFailed("EwfFileHeaderV1", e))?;

    let section_size = if *sd.size() > 0x4c {
        /* header size */
        *sd.size() - 0x4c
    }
    else {
        0
    };

    let section_type_full = sd.type_string();
    let section_type = section_type_full.trim_matches(char::from(0));

    let section = match section_type {
        "disk" | "volume" =>
            Section::Volume(VolumeSection::new(io, section_size, ignore_checksums)?),
        "table" =>
            Section::Table(read_table(io, section_size, ignore_checksums)?),
        "sectors" => Section::Sectors(io.pos() as u64 + section_size),
        "hash" => Section::Hash(get_hash_section(io, ignore_checksums)?),
        "digest" => Section::Digest(get_digest_section(io, ignore_checksums)?),
        "done" => Section::Done,
        _ => Section::Other
    };

    let section_offset = *sd.next_offset() as usize;

    Ok((section_offset, section))
}

fn get_hash_section(
    io: &BytesReader,
    ignore_checksums: bool,
) -> Result<OptRc<EwfHashSection>, LibError> {
    let hash_section =
        EwfHashSection::read_into::<_, EwfHashSection>(io, None, None)
            .map_err(|e| LibError::DeserializationFailed("EwfHashSection", e))?;

    if !ignore_checksums {
        checksum_ok(
            "Hash section",
            io,
            &hash_section._io(),
            *hash_section.checksum(),
        )?;
    }

    Ok(hash_section.clone())
}

fn get_digest_section(
    io: &BytesReader,
    ignore_checksums: bool,
) -> Result<OptRc<EwfDigestSection>, LibError> {
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

    Ok(digest_section.clone())
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

    let io_offsets = Clone::clone(io);
    let mut data_offset: u64;
    let mut chunks: Vec<Chunk> = Vec::new();
    for _ in 0..*table_section.entry_count() {
        let entry = io.read_u4le().map_err(IoError::ReadError)?;
        data_offset = (entry & 0x7fffffff) as u64;
        data_offset += *table_section.table_base_offset();
        chunks.push(Chunk {
            data_offset,
            compressed: (entry & 0x80000000) > 0,
            end_offset: None,
        });
    }

    if !ignore_checksums {
        // table footer
        let crc_stored = io.read_u4le().map_err(IoError::ReadError)?;

        let crc = checksum_reader(
            &io_offsets,
            *table_section.entry_count() as usize * 4
        )?;

        if crc != crc_stored {
            return Err(LibError::BadChecksum(
                "Table offset array".into(),
                crc,
                crc_stored
            ));
        }
    }

    Ok(chunks)
}

#[derive(Debug, Default)]
pub struct VolumeSection {
    pub chunk_count: u32,
    pub sector_per_chunk: u32,
    pub bytes_per_sector: u32,
    pub total_sector_count: u64
}

impl VolumeSection {
    pub fn new(io: &BytesReader, size: u64, ignore_checksums: bool) -> Result<Self, LibError> {
        // read volume section
        if size == 1052 {
            let vol_section =
                EwfVolume::read_into::<_, EwfVolume>(io, None, None)
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
                sector_per_chunk: *vol_section.sector_per_chunk(),
                bytes_per_sector: *vol_section.bytes_per_sector(),
                total_sector_count: *vol_section.number_of_sectors(),
            };
            Ok(vs)
        }
        else if size == 94 {
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
                sector_per_chunk: *vol_section.sector_per_chunk(),
                bytes_per_sector: *vol_section.bytes_per_sector(),
                total_sector_count: *vol_section.number_of_sectors() as u64,
            };
            Ok(vs)
        }
        else {
            Err(LibError::UnexpectedVolumeSize(size))
        }
    }

    pub fn chunk_size(&self) -> usize {
        self.sector_per_chunk as usize * self.bytes_per_sector as usize
    }

    pub fn max_offset(&self) -> usize {
        self.total_sector_count as usize * self.bytes_per_sector as usize
    }
}

pub struct SectionIterator<'a> {
    io: &'a BytesReader,
    current_offset: usize,
    ignore_checksums: bool
}

impl<'a> SectionIterator<'a> {
    pub fn new(
        io: &'a BytesReader,
        ignore_checksums: bool
    ) -> Self {
        Self {
            io,
            current_offset: io.pos(),
            ignore_checksums
        }
    }
}

impl Iterator for SectionIterator<'_> {
    type Item = Result<Section, LibError>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.current_offset < self.io.size() {
            if let Err(e) = self.io.seek(self.current_offset) {
                return Some(Err(
                    LibError::IoError(
                        IoError::SeekError(self.current_offset, e)
                    )
                ))
            }

            match read_section(self.io, self.ignore_checksums) {
                Ok((section_offset, section)) => {
                    self.current_offset = if self.current_offset == section_offset {
                        // ensure that the next() next is None
                        self.io.size()
                    }
                    else {
                        // otherwise advance to end of section
                        section_offset
                    };

                    Some(Ok(section))
                },
                Err(e) => Some(Err(e))
            }
        }
        else {
            None
        }
    }
}
