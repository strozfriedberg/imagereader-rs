use std::{
    ffi::OsStr,
    path::{Path, PathBuf}
};
use itertools::iproduct;

#[allow(clippy::manual_is_ascii_check)]
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
#[error("File {0} has an unrecognized extension")]
pub struct UnrecognizedExtension(PathBuf);

fn validate_proto_extension<T: AsRef<Path>>(
    path: T,
    ) -> Result<String, UnrecognizedExtension>
{
    path.as_ref()
        .extension()
        .map(OsStr::to_string_lossy)
        .as_deref()
        .map(str::to_ascii_uppercase)
        .filter(|ext| valid_proto_segment_ext(ext))
        .ok_or(UnrecognizedExtension(path.as_ref().into()))
}

trait ExistsChecker {
    fn is_file<T: AsRef<Path>>(&mut self, path: T) -> bool;
}

struct FileChecker;

impl ExistsChecker for FileChecker {
    fn is_file<T: AsRef<Path>>(&mut self, path: T) -> bool {
        path.as_ref().is_file()
    }
}

fn replace_extension<T: AsRef<Path>>(path: T, ext: &str) -> Option<PathBuf> {
    // TODO: use Path::with_added_extension() once it's available
    let path = path.as_ref();
    let stem = path.file_stem()?;
    let mut repl = path.parent()
        .map(|p| p.join(stem))
        .or_else(|| Some(Path::new(stem).to_path_buf()))?
        .into_os_string();
    repl.push(".");
    repl.push(ext);
    Some(repl.into())
}

fn validate_segment_path<T: AsRef<Path>, C: ExistsChecker>(
    base_path: T,
    ext: &str,
    checker: &mut C
) -> Option<PathBuf>
{
    let base_path = base_path.as_ref();

    // Hilariously, EnCase will create .E02 etc. if you start with
    // .e01, so the extensions can actually differ in case through
    // the sequence...
    let seg_path_uc = replace_extension(base_path, ext)?;
    if checker.is_file(&seg_path_uc) {
        Some(seg_path_uc)
    }
    else {
        let seg_path_lc = replace_extension(
            base_path,
            &ext.to_ascii_lowercase()
        )?;
        if checker.is_file(&seg_path_lc) {
            Some(seg_path_lc)
        }
        else {
            None
        }
    }
}

fn find_segment_paths_impl<T: AsRef<Path>, C: ExistsChecker>(
    proto_path: T,
    mut checker: C
) -> Result<impl Iterator<Item = PathBuf>, UnrecognizedExtension>
{
    let proto_path = proto_path.as_ref();

    // Get the extension from the prototype path
    let proto_ext = validate_proto_extension(proto_path)?;

    // Get first char of extension; probably cannot fail
    let ext_start = proto_ext.chars().next()
        .ok_or(UnrecognizedExtension(proto_path.into()))?;

    let base_path = replace_extension(proto_path, "")
        .ok_or(UnrecognizedExtension(proto_path.into()))?
        .to_path_buf();

    // Get the segment paths
    Ok(
        segment_ext_iter(ext_start)
            .map_while(move |ext|
                validate_segment_path(&base_path, &ext, &mut checker)
            )
    )
}

pub fn find_segment_paths<T: AsRef<Path>>(
    proto_path: T
) -> Result<impl Iterator<Item = PathBuf>, UnrecognizedExtension>
{
    find_segment_paths_impl(proto_path, FileChecker)
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
                UnrecognizedExtension(path.into())
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
            (PathBuf::from("a/img.e02"), "E02", SeqChecker::new([false, true])),
            (PathBuf::from("a/b.c.E01"), "E01", SeqChecker::new([true, false]))
        ];

        for (p, exp_ext, mut ch) in good {
            assert_eq!(
                validate_segment_path(p.clone(), exp_ext, &mut ch).unwrap(),
                p
            );
        }
     }

    #[test]
    fn find_segment_paths_impl_ok() {
        let cases = [
            ("a/i.E01", vec!["a/i.E01", "a/i.E02"], SeqChecker::new([true, true, false])),
            ("a/i.E02", vec!["a/i.E01", "a/i.E02"], SeqChecker::new([true, true, false])),
            ("a/i.e01", vec!["a/i.e01", "a/i.E02"], SeqChecker::new([false, true, true])),
            ("a/i.e02", vec!["a/i.E01", "a/i.e02"], SeqChecker::new([true, false, true])),
            ("a/i.j.e02", vec!["a/i.j.E01", "a/i.j.e02"], SeqChecker::new([true, false, true]))
        ];

        for (proto, paths, ch) in cases {
            let exp_paths = paths.iter().map(PathBuf::from).collect::<Vec<_>>();

            // Iterator doesn't impl Debug, so we need to map it
            // to something that does for the failure case
            let act_paths = find_segment_paths_impl(proto, ch)
                .map(Iterator::collect::<Vec<_>>);

            assert_eq!(act_paths.unwrap(), exp_paths);
        }
    }

    #[test]
    fn find_segment_paths_impl_err() {
        let cases = [
            ("", TrueChecker, UnrecognizedExtension("".into())),
            ("a/i", TrueChecker, UnrecognizedExtension("a/i".into())),
            ("a/i.", TrueChecker, UnrecognizedExtension("a/i.".into())),
            ("a/i.E00", TrueChecker, UnrecognizedExtension("a/i.E00".into())),
        ];

        for (proto, ch, err) in cases {
            // Iterator doesn't impl Debug, so we need to map it
            // to something that does for the failure case
            let act_paths = find_segment_paths_impl(proto, ch)
                .map(Iterator::collect::<Vec<_>>);

            assert_eq!(act_paths.unwrap_err(), err);
        }
    }
}
