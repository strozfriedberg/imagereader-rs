use crate::checksum::{checksum_ok, checksum_reader};
use crate::e01_reader::{Chunk, E01Error, FuckOffKError, Segment, VolumeSection};
use crate::generated::ewf_digest_section::EwfDigestSection;
use crate::generated::ewf_hash_section::EwfHashSection;
use crate::generated::ewf_section_descriptor_v1::EwfSectionDescriptorV1;
use crate::generated::ewf_table_header::EwfTableHeader;

use kaitai::{BytesReader, KStream, KStruct, OptRc};

pub enum Section {
    Volume(VolumeSection),
    Table(Vec<Chunk>),
    Sectors(u64),
    Hash(OptRc<EwfHashSection>),
    Digest(OptRc<EwfDigestSection>),
    Done,
    Other
}

pub fn read_section(
    io: &BytesReader,
    ignore_checksums: bool
) -> Result<(usize, Section), E01Error> {

    let sd = EwfSectionDescriptorV1::read_into::<_, EwfSectionDescriptorV1>(io, None, None)
        .map_err(|e| E01Error::DeserializationFailed { name: "EwfFileHeaderV1".into(), source: FuckOffKError(e) })?;

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
            Section::Volume(VolumeSection::new(&io, section_size, ignore_checksums)?),
        "table" =>
            Section::Table(read_table(&io, section_size, ignore_checksums)?),
        "sectors" => Section::Sectors(io.pos() as u64 + section_size),
        "hash" => Section::Hash(get_hash_section(&io, ignore_checksums)?),
        "digest" => Section::Digest(get_digest_section(&io, ignore_checksums)?),
        "done" => Section::Done,
        _ => Section::Other
    };

    let section_offset = *sd.next_offset() as usize;

    Ok((section_offset, section))
}

fn get_hash_section(
    io: &BytesReader,
    ignore_checksums: bool,
) -> Result<OptRc<EwfHashSection>, E01Error> {
    let hash_section =
        EwfHashSection::read_into::<_, EwfHashSection>(io, None, None)
            .map_err(|e| E01Error::DeserializationFailed { name: "EwfHashSection".into(), source: FuckOffKError(e) })?;

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
) -> Result<OptRc<EwfDigestSection>, E01Error> {
    let digest_section = EwfHashSection::read_into::<_, EwfDigestSection>(io, None, None)
        .map_err(|e| E01Error::DeserializationFailed { name: "EwfDigestSection".into(), source: FuckOffKError(e) })?;

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
) -> Result<Vec<Chunk>, E01Error> {
    let table_section = EwfTableHeader::read_into::<_, EwfTableHeader>(io, None, None)
        .map_err(|e| E01Error::DeserializationFailed { name: "EwfTableHeader".into(), source: FuckOffKError(e) })?;

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
        let entry = io.read_u4le().map_err(|e|
            E01Error::ReadError { source: FuckOffKError(e) })?;
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
        let crc_stored = io.read_u4le().map_err(|e|
            E01Error::ReadError { source: FuckOffKError(e) })?;

        let crc = checksum_reader(
            &io_offsets,
            *table_section.entry_count() as usize * 4
        )?;

        if crc != crc_stored {
            return Err(E01Error::BadChecksum(
                "Table offset array".into(),
                crc,
                crc_stored
            ));
        }
    }

    Ok(chunks)
}
