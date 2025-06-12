use std::path::{Path, PathBuf};
use itertools::iproduct; 

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

pub fn find_segment_paths<T: AsRef<Path>>(
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

#[cfg(test)]
mod test {
    use super::*;

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
            "YYZ",
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
