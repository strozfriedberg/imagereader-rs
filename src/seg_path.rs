use glob::{GlobError, PatternError};
use std::{
    cmp::Ordering,
    ffi::OsStr,
    path::{Path, PathBuf}
};
use itertools::iproduct;

fn valid_segment_ext(ext: &str) -> bool {
    let ext = ext.to_ascii_uppercase();
    let mut ext = ext.chars();

    (match ext.next().unwrap_or('!') {
        'E'..='Z' => match ext.next().unwrap_or('!') {
            // 01 - 09 ; 00 is not legal
            '0' => matches!(ext.next().unwrap_or('!'), '1'..='9'),
            // 10 - 99
            '1'..='9' => matches!(ext.next().unwrap_or('!'), '0'..='9'),
            // AA - ZZ
            'A'..='Z' => matches!(ext.next().unwrap_or('!'), 'A'..='Z'),
            _ => false
        },
        _ => false
    }) && ext.next().is_none() // we had three characters
}

// Prototype segment paths are used as the starting point for path globbing.
// They must have valid segment extensions and also start with E, L, or S.
fn valid_proto_segment_ext(ext: &str) -> bool {
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
    #[error("File {0} is case-insensitively ambiguous")]
    DuplicateSegmentFile(PathBuf),
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

fn validate_segment_path(
    p: PathBuf,
    exp_ext: &str
) -> Result<PathBuf, SegmentGlobError>
{
    match p.extension() {
        Some(ext) => {
            let uc_ext = ext.to_ascii_uppercase();
            let uc_ext = uc_ext.to_string_lossy();

            if !valid_segment_ext(&uc_ext) {
                Err(SegmentGlobError::UnrecognizedExtension(p))
            }
            else {
                match (*uc_ext).cmp(&exp_ext) {
                    // yay, we got a good path
                    Ordering::Equal => Ok(p),

                    // we're expecting a segment later in the sequence
                    // than the one we got; we have a case-insensitive
                    // duplicate segment (e.g., e02 and E02 both exist)
                    Ordering::Less =>
                        Err(SegmentGlobError::DuplicateSegmentFile(p)),

                    // we're expecting a segment earlier in the sequence
                    // than the one we got => a segment is missing
                    Ordering::Greater =>
                        Err(SegmentGlobError::MissingSegmentFile(p))
                }
            }
        },
        // wtf, how did we get no extension when the glob has one?
        None => Err(SegmentGlobError::UnrecognizedExtension(p))
    }
}

fn validate_segment_paths<T: IntoIterator<Item = Result<PathBuf, GlobError>>>(
    globbed_paths: T,
    ext_start: char
) -> Result<impl Iterator<Item = PathBuf>, SegmentGlobError>
{
    let mut segment_paths = vec![];

    // this is the sequence of extensions segments must have
    let ext_sequence = segment_ext_iter(ext_start);

    for (p, exp_ext) in globbed_paths.into_iter().zip(ext_sequence) {
        match p {
            Ok(p) => segment_paths.push(validate_segment_path(p, &exp_ext)?),
            // glob couldn't read this file for some reason
            Err(e) => return Err(SegmentGlobError::GlobError(e))
        }
    }

    Ok(segment_paths.into_iter())
}

fn validate_proto_extension<T: AsRef<Path>>(
    path: T,
) -> Result<String, SegmentGlobError>
{
    path.as_ref()
        .extension()
        .map(OsStr::to_string_lossy)
        .as_deref()
        .map(str::to_ascii_uppercase)
        .filter(|ext| valid_proto_segment_ext(&ext))
        .ok_or(SegmentGlobError::UnrecognizedExtension(path.as_ref().into()))
}

pub trait Globber {
    fn glob_segment_paths<T: AsRef<Path>>(
        self,
        base: T,
        ext_start: char
    ) -> Result<impl Iterator<Item = Result<PathBuf, GlobError>>, PatternError>;
}

pub struct FileGlobber;

impl Globber for FileGlobber {
    fn glob_segment_paths<T: AsRef<Path>>(
        self,
        base: T,
        ext_start: char
    ) -> Result<impl Iterator<Item = Result<PathBuf, GlobError>>, PatternError>
    {
        // Make a pattern where the extension is case-insensitive, but the
        // base is not. Case insensitively matching the base is wrong.
        //
        // Hilariously, EnCase will create .E02 etc. if you start with
        // .e01, so the extensions can actually differ in case through
        // the sequence...
        let glob_pattern = format!(
            "{}.[{}-Z{}-z][0-9A-Za-z][0-9A-Za-z]",
            base.as_ref().display(),
            ext_start.to_ascii_uppercase(),
            ext_start.to_ascii_lowercase()
        );

        glob::glob(&glob_pattern)
    }
}

pub fn find_segment_paths<T: AsRef<Path>, G: Globber>(
    proto_path: T,
    globber : G
) -> Result<impl Iterator<Item = PathBuf>, SegmentGlobError>
{
    let proto_path = proto_path.as_ref();

    // Get the extension from the prototype path and check it's valid
    let ext = validate_proto_extension(proto_path)?;

    // Get the base path and initial character of extension
    let base = proto_path.with_extension("");
    let ext_start = ext.chars().next()
        .ok_or(SegmentGlobError::UnrecognizedExtension(proto_path.into()))?;

    // Glob the segment paths
    let globbed_paths = globber.glob_segment_paths(base, ext_start)
        .map_err(|e| SegmentGlobError::PatternError {
            path: proto_path.into(),
            source: e
        })?;

    // Validate what was globbed
    validate_segment_paths(globbed_paths, ext_start)
}

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn valid_segment_ext_ok() {
        let good = [
            "E01",
            "L01",
            "S01",
            "E99",
            "EAA",
            "EZZ",
            "EZZ",
            "FAA",
            "YYZ",
            "ZZZ"
        ];

        for ext in good {
            assert!(valid_segment_ext(ext));
            assert!(valid_segment_ext(&ext.to_ascii_lowercase()));
        }
    }

    #[test]
    fn valid_segment_ext_bad() {
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
    fn valid_proto_segment_ext_ok() {
        // prototype segment extensions must start with E, L, or S
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
            assert!(valid_proto_segment_ext(ext));
            assert!(valid_proto_segment_ext(&ext.to_ascii_lowercase()));
        }
    }

    #[test]
    fn valid_proto_segment_ext_bad() {
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
            assert!(!valid_proto_segment_ext(ext));
        }
    }

    #[test]
    fn validate_proto_extension_ok() {
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
            assert_eq!(
                validate_proto_extension(format!("img.{ext}")).unwrap(),
                ext
            );
        }
    }

    #[test]
    fn validate_proto_extension_bad() {
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
            assert!(matches!(
                validate_proto_extension(format!("img.{ext}")).unwrap_err(),
                SegmentGlobError::UnrecognizedExtension(_)
            ));
        }
    }

    #[test]
    fn segment_ext_iter_boundaries() {
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

    #[test]
    fn validate_segment_path_ok() {
        let good = [
            (PathBuf::from("a/img.E01"), "E01"),
            (PathBuf::from("a/img.E02"), "E02"),
            (PathBuf::from("a/img.e02"), "E02")
        ];

        for (p, exp_ext) in good {
            assert_eq!(
                validate_segment_path(p.clone(), exp_ext).unwrap(),
                p
            );
        }
    }

    #[test]
    fn validate_segment_path_unrecognized() {
        let unrecognized = [
            (PathBuf::from(""), "E01"),
            (PathBuf::from("img"), "E01"),
            (PathBuf::from("a/img.E00"), "E01"),
        ];

        for (p, exp_ext) in unrecognized {
            assert!(matches!(
                validate_segment_path(p, exp_ext).unwrap_err(),
                SegmentGlobError::UnrecognizedExtension(_)
            ));
        }
    }

    #[test]
    fn validate_segment_path_duplicate() {
        let duplicate = [
            (PathBuf::from("img.E01"), "E02")
        ];

        for (p, exp_ext) in duplicate {
            assert!(matches!(
                validate_segment_path(p, exp_ext).unwrap_err(),
                SegmentGlobError::DuplicateSegmentFile(_)
            ));
        }
    }

    #[test]
    fn validate_segment_path_missing() {
        let missing = [
            (PathBuf::from("img.E02"), "E01")
        ];

        for (p, exp_ext) in missing {
            assert!(matches!(
                validate_segment_path(p, exp_ext).unwrap_err(),
                SegmentGlobError::MissingSegmentFile(_)
            ));
        }
    }
}
