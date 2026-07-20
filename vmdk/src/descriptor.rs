use regex::Regex;
use std::{
    io::{BufRead, BufReader, Read, Seek, SeekFrom},
    sync::LazyLock,
};

use crate::errors::{DescriptorError, OpenErrorKind};

const SECTOR_SIZE: u64 = 512;

pub fn read_descriptor_internal<R>(src: &mut R, offset: u64) -> Result<String, std::io::Error>
where
    R: Read + Seek,
{
    let mut buf = vec![];

    src.seek(SeekFrom::Start(offset * SECTOR_SIZE))?;

    let mut r = BufReader::new(src.take(20 * SECTOR_SIZE));
    r.read_until(0, &mut buf)?;

    // read_until includes the NUL delimiter when it finds one. Strip it only
    // if it's actually there: an offset at or past EOF reads zero bytes (so
    // `len - 1` would underflow), and a buffer that hit the 20-sector cap
    // without a NUL must keep its last real byte.
    if buf.last() == Some(&0) {
        buf.pop();
    }

    Ok(String::from_utf8_lossy(&buf).into())
}

pub fn read_descriptor_file<R>(src: R) -> Result<String, OpenErrorKind>
where
    R: Read,
{
    // Read a line at a time until we know we have a descriptor file,
    // to avoid reading a giant file which is not a descriptor file
    // into memory.

    let mut r = BufReader::new(src);
    let mut desc = String::new();
    let mut line = String::new();

    loop {
        line.clear();
        // EOF before the header line: this is not a descriptor file.
        if r.read_line(&mut line)? == 0 {
            return Err(OpenErrorKind::DescriptorError(
                DescriptorError::UnrecognizedDescriptor,
            ));
        }
        desc += &line;

        match line.as_str().trim_end() {
            "# Disk DescriptorFile" => {
                // this is a descriptor file, read the rest
                r.read_to_string(&mut desc)?;
                return Ok(desc);
            }
            "" => {}
            _ => {
                return Err(OpenErrorKind::DescriptorError(
                    DescriptorError::UnrecognizedDescriptor,
                ));
            }
        }
    }
}

pub fn extract_parent_fn_hint(descriptor: &str) -> Option<String> {
    static PAT: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r#"^parentFileNameHint="([^"]+)"#).expect("bad regex"));

    for line in descriptor.lines() {
        if let Some(captures) = PAT.captures(line) {
            return Some(captures[1].to_string());
        }
    }
    None
}

#[cfg(test)]
mod test {
    use super::*;
    use std::io::Cursor;

    /// An offset at or past EOF reads zero bytes; the old `len - 1` underflowed
    /// and panicked. It must return an empty descriptor instead.
    #[test]
    fn read_descriptor_internal_past_eof_is_empty_not_panic() {
        let mut cur = Cursor::new(Vec::<u8>::new());
        assert_eq!(read_descriptor_internal(&mut cur, 5).unwrap(), "");
    }

    /// When the descriptor data has no trailing NUL (e.g. it fills the sector
    /// cap), the last real byte must be preserved, not dropped.
    #[test]
    fn read_descriptor_internal_keeps_last_byte_without_nul() {
        let mut cur = Cursor::new(b"hello".to_vec());
        assert_eq!(read_descriptor_internal(&mut cur, 0).unwrap(), "hello");
    }

    /// A NUL delimiter is still stripped, and bytes past it are ignored.
    #[test]
    fn read_descriptor_internal_strips_nul_delimiter() {
        let mut data = b"abc".to_vec();
        data.push(0);
        data.extend_from_slice(b"trailing");
        let mut cur = Cursor::new(data);
        assert_eq!(read_descriptor_internal(&mut cur, 0).unwrap(), "abc");
    }

    #[test]
    fn test_read_descriptor_file_ok() {
        let desc = r#"
# Disk DescriptorFile
version=1
encoding="UTF-8"
CID=8f67ca74
parentCID=0172e8a4
createType="vmfsSparse"
parentFileNameHint="vmfs_thick.vmdk"
# Extent description
RW 4096 VMFSSPARSE "vmfs_thick-000001-delta.vmdk"

# The Disk Data Base
#DDB

ddb.longContentID = "4b98b55ba6a6bc2e8fd6eb368f67ca74"
"#;

        assert_eq!(read_descriptor_file(desc.as_bytes()).unwrap(), desc);
    }

    #[test]
    fn test_read_descriptor_file_bad() {
        let desc = r#"


Bogus crap
"#;

        assert!(matches!(
            read_descriptor_file(desc.as_bytes()).unwrap_err(),
            OpenErrorKind::DescriptorError(DescriptorError::UnrecognizedDescriptor)
        ));
    }

    #[test]
    fn test_read_descriptor_file_blank_lines_terminates() {
        // A file of only blank lines must terminate with an error, not spin.
        let desc = "\n\n\n\n\n\n\n\n";
        assert!(matches!(
            read_descriptor_file(desc.as_bytes()).unwrap_err(),
            OpenErrorKind::DescriptorError(DescriptorError::UnrecognizedDescriptor)
        ));
    }
}
