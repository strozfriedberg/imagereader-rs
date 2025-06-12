use flate2::read::ZlibDecoder;
use itertools::iproduct;
use std::{
    convert::TryFrom,
    io::Read
};
use std::path::{Path, PathBuf};

extern crate kaitai;
use self::kaitai::*;

use crate::generated::ewf_file_header_v1::*;
use crate::generated::ewf_file_header_v2::*;
use crate::generated::ewf_section_descriptor_v1::*;
//use crate::generated::ewf_section_descriptor_v2::*;
use crate::generated::ewf_digest_section::*;
use crate::generated::ewf_hash_section::*;
use crate::generated::ewf_table_header::*;
use crate::generated::ewf_volume::*;
use crate::generated::ewf_volume_smart::*;

#[derive(Debug)]
pub struct FuckOffKError(KError);

impl std::fmt::Display for FuckOffKError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:?}", self.0)
    }
}

impl std::error::Error for FuckOffKError {}

#[derive(Debug, thiserror::Error)]
pub enum E01Error {
    #[error("Decompression failed")]
    DecompressionFailed(#[source] std::io::Error),
    #[error("Error while deserializing {name} struct: {source}")]
    DeserializationFailed {
        name: String,
        #[source]
        source: FuckOffKError
    },
    #[error("{source}")]
    OpenError {
        #[source]
        source: FuckOffKError
    },
    #[error("{source}")]
    ReadError {
        #[source]
        source: FuckOffKError
    },
    #[error("{source}")]
    SeekError {
        #[source]
        source: FuckOffKError
    },
    #[error("Segment file {file}, seek to {offset} failed: {source}")]
    SegmentSeekError {
        file: PathBuf,
        offset: usize,
        #[source]
        source: FuckOffKError
    },
    #[error("Unknown volume size: {0}")]
    UnknownVolumeSize(u64),
    #[error("{0}")]
    IoError(#[from] std::io::Error),
    #[error("Invalid segment file")]
    InvalidSegmentFile,
    #[error("{0} checksum failed, calculated {1}, expected {2}")]
    BadChecksum(String, u32, u32),
    #[error("Unknown compression method value: {0}")]
    UnknownCompressionMethod(u16),
    #[error("Requested chunk number {0} is wrong")]
    BadChunkNumber(usize),
    #[error("Requested offset {0} is over max offset {1}")]
    OffsetBeyondEnd(usize, usize),
    #[error("Can't find file: {0}")]
    FileNotFound(PathBuf),
    #[error("Invalid EWF file: {0}")]
    InvalidFile(PathBuf),
    #[error("Glob error")]
    GlobError,
    #[error("Invalid filename")]
    InvalidFilename,
    #[error("Invalid extension")]
    InvalidExtension,
    #[error("Missing volume section")]
    MissingVolumeSection,
    #[error("Missing some segment file")]
    MissingSegmentFile,
    #[error("Too many chunks")]
    TooManyChunks,
    #[error("Too few chunks")]
    TooFewChunks,
    #[error("Duplicate volume section")]
    DuplicateVolumeSection
}

#[derive(Debug)]
pub enum CompressionMethod {
    None = 0,
    Deflate = 1,
    Bzip = 2,
}

impl TryFrom<u16> for CompressionMethod {
    type Error = E01Error;

    fn try_from(v: u16) -> Result<Self, Self::Error> {
        match v {
            x if x == Self::None as u16 => Ok(Self::None),
            x if x == Self::Deflate as u16 => Ok(Self::Deflate),
            x if x == Self::Bzip as u16 => Ok(Self::Bzip),
            _ => Err(E01Error::UnknownCompressionMethod(v))
        }
    }
}

#[derive(Debug)]
pub struct SegmentFileHeader {
    pub major_version: u8,
    pub minor_version: u8,
    pub compr_method: CompressionMethod,
    pub segment_number: u16,
}

impl SegmentFileHeader {
    pub fn new(io: &BytesReader) -> Result<Self, E01Error> {
        let first_bytes = io
            .read_bytes(8)
            .map_err(|e| E01Error::ReadError { source: FuckOffKError(e) })?;

        io.seek(0)
            .map_err(|e| E01Error::SeekError { source: FuckOffKError(e) })?;

        // read file header

        if first_bytes == [0x45, 0x56, 0x46, 0x09, 0x0d, 0x0a, 0xff, 0x00] // EWF, EWF-E01, EWF-S01
            || first_bytes == [0x4c, 0x56, 0x46, 0x09, 0x0d, 0x0a, 0xff, 0x00]
        // EWF-L01
        // V1
        {
            match EwfFileHeaderV1::read_into::<_, EwfFileHeaderV1>(io, None, None) {
                Ok(h) => {
                    Ok(SegmentFileHeader {
                        major_version: 1,
                        minor_version: 0,
                        compr_method: CompressionMethod::Deflate,
                        segment_number: *h.segment_number(),
                    })
                }
                Err(e) => {
                    Err(E01Error::DeserializationFailed { name: "EwfFileHeaderV1".into(), source: FuckOffKError(e) })
                }
            }
        } else if first_bytes == [0x45, 0x56, 0x46, 0x32, 0x0d, 0x0a, 0x81, 0x00] // EVF2
            || first_bytes == [0x4c, 0x45, 0x46, 0x32, 0x0d, 0x0a, 0x81, 0x00]
        // LEF2
        // V2
        {
            match EwfFileHeaderV2::read_into::<_, EwfFileHeaderV2>(io, None, None) {
                Ok(h) => {
                    Ok(SegmentFileHeader {
                        major_version: *h.major_version(),
                        minor_version: *h.minor_version(),
                        compr_method: (*h.compression_method()).try_into()?,
                        segment_number: *h.segment_number(),
                    })
                }
                Err(e) => {
                    Err(E01Error::DeserializationFailed { name: "EwfFileHeaderV2".into(), source: FuckOffKError(e) })
                }
            }
        }
        else {
            Err(E01Error::InvalidSegmentFile)
        }
    }
}

fn checksum(bytes: &[u8]) -> Result<u32, E01Error> {
    Ok(adler32::adler32(std::io::Cursor::new(bytes))?)
}

fn checksum_reader(
    reader: &BytesReader,
    len: usize
) -> Result<u32, E01Error>
{
    checksum(
        &reader
            .read_bytes(len)
            .map_err(|e| E01Error::ReadError { source: FuckOffKError(e) })?
    )
}

fn checksum_ok(
    section_type: &str,
    io: &BytesReader,
    section_io: &BytesReader,
    crc_stored: u32,
) -> Result<(), E01Error>
{
    let crc = checksum_reader(section_io, io.pos() - section_io.pos() - 4)?;
    match crc == crc_stored {
        true => Ok(()),
        false => Err(E01Error::BadChecksum(section_type.into(), crc, crc_stored))
    }
}

#[derive(Debug, Default)]
pub struct VolumeSection {
    pub chunk_count: u32,
    pub sector_per_chunk: u32,
    pub bytes_per_sector: u32,
    pub total_sector_count: u64
}

impl VolumeSection {
    pub fn new(io: &BytesReader, size: u64, ignore_checksums: bool) -> Result<Self, E01Error> {
        // read volume section
        if size == 1052 {
            let vol_section =
                EwfVolume::read_into::<_, EwfVolume>(io, None, None)
                    .map_err(|e| E01Error::DeserializationFailed { name: "EwfVolume".into(), source: FuckOffKError(e) })?;

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
                .map_err(|e| E01Error::DeserializationFailed { name: "EwfVolumeSmart".into(), source: FuckOffKError(e) })?;

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
            Err(E01Error::UnknownVolumeSize(size))
        }
    }

    pub fn chunk_size(&self) -> usize {
        self.sector_per_chunk as usize * self.bytes_per_sector as usize
    }

    pub fn max_offset(&self) -> usize {
        self.total_sector_count as usize * self.bytes_per_sector as usize
    }
}

#[derive(Debug)]
pub struct E01Reader {
    volume: VolumeSection,
    segments: Vec<Segment>,
    ignore_checksums: bool,
    stored_md5: Option<Vec<u8>>,
    stored_sha1: Option<Vec<u8>>,
}

#[derive(Debug)]
pub struct Segment {
    io: BytesReader,
    _header: SegmentFileHeader,
    chunks: Vec<Chunk>,
    end_of_sectors: u64,
}

#[derive(Debug)]
struct Chunk {
    data_offset: u64,
    compressed: bool,
    end_offset: Option<u64>,
}

impl Segment {
    fn read_table(
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

            let section =
                EwfSectionDescriptorV1::read_into::<_, EwfSectionDescriptorV1>(&io, None, None)
                    .map_err(|e| E01Error::DeserializationFailed { name: format!("Segment file {} EwfFileHeaderV1", f.as_ref().to_string_lossy()), source: FuckOffKError(e) })?;

            let section_offset = *section.next_offset() as usize;
            let section_size = if *section.size() > 0x4c
            /* header size */
            {
                *section.size() - 0x4c
            } else {
                0
            };
            let section_type_full = section.type_string();
            let section_type = section_type_full.trim_matches(char::from(0));

            if section_type == "disk" || section_type == "volume" {
                *volume = Some(VolumeSection::new(&io, section_size, ignore_checksums)?);
            }

            if section_type == "table" {
                chunks.extend(Segment::read_table(&io, section_size, ignore_checksums)?);
                let chunks_len = chunks.len();
                chunks[chunks_len - 1].end_offset = Some(end_of_sectors);
            }

            if section_type == "sectors" {
                end_of_sectors = io.pos() as u64 + section_size;
            }

            if section_type == "hash" {
                let hash_section = Self::get_hash_section(&io, ignore_checksums)?;
                *stored_md5 = Some(hash_section.md5().clone());
            }

            if section_type == "digest" {
                let digest_section = Self::get_digest_section(&io, ignore_checksums)?;
                *stored_md5 = Some(digest_section.md5().clone());
                *stored_sha1 = Some(digest_section.sha1().clone());
            }

            if current_offset == section_offset || section_type == "done" {
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

    fn read_chunk(
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
                .map_err(|e| E01Error::DecompressionFailed(e))?;
            Ok(data)
        }
    }
}

fn valid_segment_ext(ext: &str) -> bool {
    let ext = ext.to_ascii_uppercase();
    let mut ext = ext.chars();

    (match ext.next().unwrap_or('!') {
        'E'..='Z' => match ext.next().unwrap_or('!') {
            // 01 - E09
            '0' => match ext.next().unwrap_or('!') {
                // 00 is not legal
                '1'..='9' => true,
                _ => false
            },
            // 10 - 99
            '1'..='9' => match ext.next().unwrap_or('!') {
                '0'..='9' => true,
                _ => false
            },
            // AA - ZZ
            'A'..='Z' => match ext.next().unwrap_or('!') {
                'A'..='Z' => true,
                _ => false
            },
            _ => false
        },
        _ => false
    }) && ext.next().is_none() // we had three characters
}

fn valid_example_segment_ext(ext: &str) -> bool {
    valid_segment_ext(ext) &&
    ['E', 'L', 'S'].contains(
        &ext
            .chars()
            .next()
            .as_ref()
            .map(char::to_ascii_uppercase)
            .unwrap_or('!')
    )
}

fn segment_ext_iter(start: char) -> impl Iterator<Item = String> {
    // x01 to x99
    (1..=99)
        .map(move |n| format!("{}{:02}", start, n))
        // xAA - ZZZ
        .chain(
            iproduct!(start..='Z', 'A'..='Z', 'A'..='Z')
                .map(|t| format!("{}{}{}", t.0, t.1, t.2))
        )
}

#[derive(Debug, thiserror::Error)]
pub enum SegmentGlobError {
    #[error("File {0} is ambiguous with file {1}")]
    DuplicateSegmentFile(PathBuf, PathBuf),
    #[error("Failed to read file {}: {}", .0.path().display(), .0)]
    GlobError(#[from] glob::GlobError),
    #[error("File {0} not found")]
    MissingSegmentFile(PathBuf),
    #[error("Failed to make glob pattern for file {path}: {source}")]
    PatternError {
        path: PathBuf,
        source: glob::PatternError
    },
    #[error("File {0} has an unrecognized extension")]
    UnrecognizedExtension(PathBuf)
}

fn find_segment_paths<T: AsRef<Path>>(
    example_path: T
) -> Result<impl Iterator<Item = PathBuf>, SegmentGlobError>
{
    let example_path = example_path.as_ref();

    // Get the extension from the example path and ensure it's ok
    let uc_ext = example_path.extension()
        .ok_or(SegmentGlobError::UnrecognizedExtension(example_path.into()))?
        .to_ascii_uppercase();

    let uc_ext = uc_ext.to_string_lossy();
    if !valid_example_segment_ext(&uc_ext) {
        return Err(SegmentGlobError::UnrecognizedExtension(example_path.into()));
    }

    let base = example_path.with_extension("");
    let ext_start = uc_ext.chars().next()
        .ok_or(SegmentGlobError::UnrecognizedExtension(example_path.into()))?;

    // Make a pattern where the extension is case-insensitive, but the
    // base is not. Case insensitively matching the base is wrong.
    //
    // Hilariously, EnCase will create .E02 etc. if you start with
    // .e01, so the extensions can actually differ in case through
    // the sequence...
    let glob_pattern = format!(
        "{}.[{}-Z{}-z][0-9A-Za-z][0-9A-Za-z]",
        base.display(),
        ext_start.to_ascii_uppercase(),
        ext_start.to_ascii_lowercase()
    );

    let globbed_paths = glob::glob(&glob_pattern)
        .map_err(|e| SegmentGlobError::PatternError {
            path: example_path.into(),
            source: e
        })?;

    let mut segment_paths = vec![];

    // this is the sequence of extensions segments must have
    let ext_sequence = segment_ext_iter(ext_start);

    for (p, exp_ext) in globbed_paths.zip(ext_sequence) {
        match p {
            Ok(p) => match p.extension() {
                Some(ext) => {
                    let uc_ext = ext.to_ascii_uppercase();

                    if !valid_segment_ext(&uc_ext.to_string_lossy()) {
                        return Err(SegmentGlobError::UnrecognizedExtension(p));
                    }

                    if *uc_ext > *exp_ext {
                        // we're expecting a segment earlier in the sequence
                        // than the one we got => a segment is missing
                        return Err(SegmentGlobError::MissingSegmentFile(p))
                    }
                    else if *uc_ext < *exp_ext {
                        // we're expecting a segment later in the sequence
                        // than the one we got; we have a case-insensitive
                        // duplicate segment (e.g., e02 and E02 both exist)
                        return Err(SegmentGlobError::DuplicateSegmentFile(
                            p,
                            segment_paths.pop()
                                .expect("impossible, nothing is before E01")
                        ))
                    }

                    segment_paths.push(p);
                },
                // wtf, how did we get no extension when the glob has one?
                None => return Err(SegmentGlobError::UnrecognizedExtension(p))
            }
            // glob couldn't read this file for some reason
            Err(e) => return Err(SegmentGlobError::GlobError(e))
        }
    }

    Ok(segment_paths.into_iter())
}

impl E01Reader {


// open a list of files
// open a single file as a pattern

// Errors should be: ioerror, bad paths, bad input

/*
    fn open_impl<T: impl IntoIterator<Item = Result<PathBuf, GlobError>>, E01Error> -> Result<Self, E01Error> {

        // do all the crap here

    }

    pub fn open<T: impl IntoIterator<Item = AsRef<Path>>>(
        paths: T,
        ignore_checksums: bool
    ) -> Result<Self, E01Error>
    {

    }
*/

    pub fn open<T: AsRef<Path>>(
        f: T,
        ignore_checksums: bool
    ) -> Result<Self, E01Error>
    {
        let mut volume_opt: Option<VolumeSection> = None;
        let mut stored_md5: Option<_> = None;
        let mut stored_sha1: Option<_> = None;

        let mut segments = vec![];
        let mut chunks = 0;

        let mut segment_paths = find_segment_paths(&f)
            .or(Err(E01Error::InvalidFilename))?;

        // read first segment, volume section must be contained in it
        let sp = segment_paths.next()
            .ok_or(E01Error::InvalidFilename)?;

        let seg = Segment::read(
            sp,
            &mut volume_opt,
            &mut stored_md5,
            &mut stored_sha1,
            ignore_checksums,
        )?;

        let volume = volume_opt.ok_or(E01Error::MissingVolumeSection)?;
        let exp_chunks = volume.chunk_count as usize;

//        let mut stored_md5_unexpected = None;
//        let mut stored_sha1_unexpected = None;
        volume_opt = None;

        chunks += seg.chunks.len();
        segments.push(seg);

        // continue reading segments
        for sp in segment_paths {
            let seg = Segment::read(
                sp,
                &mut volume_opt,
//                &mut stored_md5_unexpected,
//                &mut stored_sha1_unexpected,
                &mut stored_md5,
                &mut stored_sha1,
                ignore_checksums,
            )?;

            // we should not see volume, hash, digest sections again
            if volume_opt.is_some() {
                return Err(E01Error::DuplicateVolumeSection);
            }

/*
            if stored_md5_unexpected.is_some() {
                return Err(E01Error::DuplicateMD5);
            }

            if stored_sha1_unexpected.is_some() {
                return Err(E01Error::DuplicateSHA1);
            }
*/

            chunks += seg.chunks.len();
            segments.push(seg);
        }

        if chunks > exp_chunks {
            return Err(E01Error::TooManyChunks);
        }
        else if chunks < exp_chunks {
            return Err(E01Error::TooFewChunks);
        }

/*
        let segment_paths = candidate_segments(&f)
            .ok_or(E01Error::InvalidFilename)?;

        for sp in segment_paths {
            let seg = Segment::read(
                sp,
                &mut volume_opt,
                &mut stored_md5,
                &mut stored_sha1,
                ignore_checksums,
            )?;

            chunks += seg.chunks.len();
            segments.push(seg);

            if let Some(ref volume) = volume_opt {
                let exp_chunks = volume.chunk_count as usize;
                if chunks == exp_chunks {
                    break;
                }
                else if chunks > exp_chunks {
                    return Err(E01Error::TooManyChunks)
                }
            }
        }
*/

        Ok(E01Reader {
            volume,
            segments,
            ignore_checksums,
            stored_md5,
            stored_sha1,
        })
    }

    pub fn total_size(&self) -> usize {
        self.volume.max_offset()
    }

    fn get_segment(
        &self,
        chunk_number: usize,
        chunk_index: &mut usize,
    ) -> Result<&Segment, E01Error> {
        let mut chunks = 0;
        self.segments
            .iter()
            .find(|s| {
                if chunk_number >= chunks && chunk_number < chunks + s.chunks.len() {
                    *chunk_index = chunk_number - chunks;
                    return true;
                }
                chunks += s.chunks.len();
                false
            })
            .ok_or_else(|| E01Error::BadChunkNumber(chunk_number))
    }

    pub fn read_at_offset(
        &self,
        mut offset: usize,
        buf: &mut [u8]
    ) -> Result<usize, E01Error>
    {
        let total_size = self.total_size();
        if offset > total_size {
            return Err(E01Error::OffsetBeyondEnd(offset, total_size));
        }

        let mut bytes_read = 0;
        let mut remaining_buf = &mut buf[..];

        while remaining_buf.len() > 0 && offset < total_size {
            let chunk_number = offset / self.chunk_size();
            debug_assert!(chunk_number < self.volume.chunk_count as usize);
            let mut chunk_index = 0;

            let mut data = self
                .get_segment(chunk_number, &mut chunk_index)?
                .read_chunk(
                    chunk_number,
                    chunk_index,
                    self.ignore_checksums,
                    &mut remaining_buf
                )?;

            if chunk_number * self.chunk_size() + data.len() > total_size {
                data.truncate(total_size - chunk_number * self.chunk_size());
            }

            let data_offset = offset % self.chunk_size();

            if buf.len() < bytes_read || data.len() < data_offset {
                println!("todo");
            }

            let remaining_size = std::cmp::min(buf.len() - bytes_read, data.len() - data_offset);
            remaining_buf = &mut buf[bytes_read..bytes_read + remaining_size];
            remaining_buf.copy_from_slice(&data[data_offset..data_offset + remaining_size]);

            debug_assert!(offset + remaining_size <= total_size);

            bytes_read += remaining_size;
            offset += remaining_size;
        }

        Ok(bytes_read)
    }

    pub fn chunk_size(&self) -> usize {
        self.volume.chunk_size()
    }

    pub fn get_stored_md5(&self) -> Option<&Vec<u8>> {
        self.stored_md5.as_ref()
    }

    pub fn get_stored_sha1(&self) -> Option<&Vec<u8>> {
        self.stored_sha1.as_ref()
    }
}

#[cfg(test)]
mod test {
    use super::*;

    use std::{
        path::PathBuf
    };

    #[test]
    fn valid_segment_ext_tests() {
        let good = [
            "E01",
            "L01",
            "S01",
            "E99",
            "EAA",
            "EZZ",
            "EZZ",
            "FAA",
            "ZZZ"
        ];

        for ext in good {
            assert!(valid_segment_ext(ext));
            assert!(valid_segment_ext(&ext.to_ascii_lowercase()));
        }

        let bad = [
            "",
            "E",
            "E0",
            "E00",
            "E0A",
            "EA0",
            "AbC",
            "gtfo",
            "💩"
        ];

        for ext in bad {
            assert!(!valid_segment_ext(ext));
        }
    }

    #[test]
    fn valid_example_segment_ext_tests() {
        // example segment extensions must start with E, L, or S
        let good = [
            "E01",
            "L01",
            "S01",
            "E99",
            "EAA",
            "EZZ",
            "EZZ"
        ];

        for ext in good {
            assert!(valid_example_segment_ext(ext));
            assert!(valid_example_segment_ext(&ext.to_ascii_lowercase()));
        }

        let bad = [
            "FAA",
            "ZZZ",
            "",
            "E",
            "E0",
            "E00",
            "E0A",
            "EA0",
            "AbC",
            "gtfo",
            "💩"
        ];

        for ext in bad {
            assert!(!valid_example_segment_ext(ext));
        }
    }

    #[test]
    fn segment_ext_iter_tests() {
        // check that a sample of extensions are in the expected positions
        let mut i = segment_ext_iter('E');
        assert_eq!(i.next(), Some("E01".into()));
        assert_eq!(i.next(), Some("E02".into()));
        let mut i = i.skip(96);
        assert_eq!(i.next(), Some("E99".into()));
        assert_eq!(i.next(), Some("EAA".into()));
        assert_eq!(i.next(), Some("EAB".into()));
        let mut i = i.skip(23);
        assert_eq!(i.next(), Some("EAZ".into()));
        assert_eq!(i.next(), Some("EBA".into()));
        let mut i = i.skip(648);
        assert_eq!(i.next(), Some("EZZ".into()));
        assert_eq!(i.next(), Some("FAA".into()));
        let mut i = i.skip(14194);
        assert_eq!(i.next(), Some("ZZZ".into()));
        assert_eq!(i.next(), None);
    }
}
