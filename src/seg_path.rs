use std::{
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

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum SegmentPathError {
    #[error("File {0} is case-insensitively ambiguous")]
    DuplicateSegmentFile(PathBuf),
    #[error("File {0} has an unrecognized extension")]
    UnrecognizedExtension(PathBuf)
}

fn validate_proto_extension<T: AsRef<Path>>(
    path: T,
    ) -> Result<String, SegmentPathError>
{
    path.as_ref()
        .extension()
        .map(OsStr::to_string_lossy)
        .as_deref()
        .map(str::to_ascii_uppercase)
        .filter(|ext| valid_proto_segment_ext(ext))
        .ok_or(SegmentPathError::UnrecognizedExtension(path.as_ref().into()))
}

pub trait ExistsChecker {
    fn is_file<T: AsRef<Path>>(&mut self, path: T) -> bool;
}

pub struct FileChecker;

impl ExistsChecker for FileChecker {
    fn is_file<T: AsRef<Path>>(&mut self, path: T) -> bool {
        path.as_ref().is_file()
    }
}

fn validate_segment_path<T: AsRef<Path>, C: ExistsChecker>(
    base_path: T,
    ext: &str,
    checker: &mut C
) -> Result<Option<PathBuf>, SegmentPathError>
{
    let base_path = base_path.as_ref();

    // Hilariously, EnCase will create .E02 etc. if you start with
    // .e01, so the extensions can actually differ in case through
    // the sequence...
    let seg_path_uc = base_path.with_extension(ext);
    let seg_path_lc = base_path.with_extension(ext.to_ascii_lowercase());

    match (checker.is_file(&seg_path_uc), checker.is_file(&seg_path_lc)) {
        // we found only the uppercase extension
        (true, false) => Ok(Some(seg_path_uc)),
        // we found only the lowercase extension
        (false, true) => Ok(Some(seg_path_lc)),
        // we found both extensions (!)
        (true, true) => Err(SegmentPathError::DuplicateSegmentFile(seg_path_uc)),
        // we found neither extension, maybe end of segments?
        (false, false) => Ok(None)
    }
}

fn validate_segment_paths<T: AsRef<Path>, C: ExistsChecker>(
    base_path: T,
    ext_start: char,
    checker: &mut C
) -> Result<impl Iterator<Item = PathBuf>, SegmentPathError>
{
    let mut segment_paths = vec![];

    // step through the sequence of extensions
    for ext in segment_ext_iter(ext_start) {
        match validate_segment_path(&base_path, &ext, checker) {
            Ok(Some(p)) => segment_paths.push(p),
            Ok(None) => break,
            Err(e) => return Err(e)
        }
    }

    Ok(segment_paths.into_iter())
}

pub fn find_segment_paths<T: AsRef<Path>, C: ExistsChecker>(
    proto_path: T,
    checker: &mut C
) -> Result<impl Iterator<Item = PathBuf>, SegmentPathError>
{
    let proto_path = proto_path.as_ref();

    // Get the extension from the prototype path and check it's valid
    let ext = validate_proto_extension(proto_path)?;

    // Get the base path and initial character of extension
    let base = proto_path.with_extension("");
    let ext_start = ext.chars().next()
        .ok_or(SegmentPathError::UnrecognizedExtension(proto_path.into()))?;

    validate_segment_paths(base, ext_start, checker)
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
            let path = format!("img.{ext}");
            assert_eq!(
                validate_proto_extension(&path).unwrap_err(),
                SegmentPathError::UnrecognizedExtension(path.into())
            );
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

    struct SeqChecker<S: Iterator<Item = bool>>(S);

    impl<S: Iterator<Item = bool>> SeqChecker<S> {
        fn new(seq: impl IntoIterator<Item = bool, IntoIter = S>) -> Self
        {
            Self(seq.into_iter())
        }
    }

    impl<S: Iterator<Item = bool>> ExistsChecker for SeqChecker<S> {
        fn is_file<T: AsRef<Path>>(&mut self, _path: T) -> bool {
            self.0.next().unwrap_or(false)
        }
    }

    struct TrueChecker;

    impl ExistsChecker for TrueChecker {
        fn is_file<T: AsRef<Path>>(&mut self, _path: T) -> bool {
            true
        }
    }

    #[test]
    fn validate_segment_path_ok() {
        let good = [
            (PathBuf::from("a/img.E01"), "E01", SeqChecker::new([true, false])),
            (PathBuf::from("a/img.E02"), "E02", SeqChecker::new([true, false])),
            (PathBuf::from("a/img.e02"), "E02", SeqChecker::new([false, true]))
        ];

        for (p, exp_ext, mut ch) in good {
            assert_eq!(
                validate_segment_path(p.clone(), exp_ext, &mut ch).unwrap(),
                Some(p)
            );
        }
     }

    #[test]
    fn validate_segment_path_duplicate() {
        let duplicate = [
            (PathBuf::from("img.E01"), "E01")
        ];

        let mut ch = TrueChecker; // both E01 and e01 exist

        for (p, exp_ext) in duplicate {
            assert_eq!(
                validate_segment_path(p.clone(), exp_ext, &mut ch).unwrap_err(),
                SegmentPathError::DuplicateSegmentFile(p.into())
            );
        }
    }

    #[test]
    fn find_segment_paths_ok() {
        let cases = [
            ("a/i.E01", vec!["a/i.E01", "a/i.E02"], SeqChecker::new([true, false, true, false])),
            ("a/i.E02", vec!["a/i.E01", "a/i.E02"], SeqChecker::new([true, false, true, false])),
            ("a/i.e01", vec!["a/i.e01", "a/i.E02"], SeqChecker::new([false, true, true, false])),
            ("a/i.e02", vec!["a/i.E01", "a/i.e02"], SeqChecker::new([true, false, false, true]))
        ];

        for (proto, paths, mut ch) in cases {
            let exp_paths = paths.iter().map(PathBuf::from).collect::<Vec<_>>();

            // Iterator doesn't impl Debug, so we need to map it
            // to something that does for the failure case
            let act_paths = find_segment_paths(proto, &mut ch)
                .map(Iterator::collect::<Vec<_>>);

            assert_eq!(act_paths.unwrap(), exp_paths);
        }
    }

    #[test]
    fn find_segment_paths_err() {
        let cases = [
            ("", TrueChecker, SegmentPathError::UnrecognizedExtension("".into())),
            ("a/i", TrueChecker, SegmentPathError::UnrecognizedExtension("a/i".into())),
            ("a/i.", TrueChecker, SegmentPathError::UnrecognizedExtension("a/i.".into())),
            ("a/i.E00", TrueChecker, SegmentPathError::UnrecognizedExtension("a/i.E00".into())),
            ("a/i.E01", TrueChecker, SegmentPathError::DuplicateSegmentFile("a/i.E01".into()))
        ];

        for (proto, mut ch, err) in cases {
            // Iterator doesn't impl Debug, so we need to map it
            // to something that does for the failure case
            let act_paths = find_segment_paths(proto, &mut ch)
                .map(Iterator::collect::<Vec<_>>);

            assert_eq!(act_paths.unwrap_err(), err);
        }
    }
}
