use imagesource::exists::ExistsChecker;

/// A segment the sequence needs but that is not there.
///
/// Refusing beats continuing. Before split support, opening `disk.001` produced
/// a working reader over one third of the image -- partition tables and
/// filesystem headers live in the first segment, so it parses and mounts and
/// looks right until something reads past the boundary. A named error is far
/// better than a silently short image.
#[derive(Debug, thiserror::Error)]
#[error("missing image segment: {path}")]
pub struct MissingSegment {
    pub path: String,
}

/// Splits `path` into `(stem, number, digit width)` when it ends in `.` followed
/// by digits. `disk.001` -> `("disk", 1, 3)`; `disk.raw` -> `None`.
fn split_numeric_suffix(path: &str) -> Option<(&str, u64, usize)> {
    let (stem, suffix) = path.rsplit_once('.')?;
    if suffix.is_empty() || !suffix.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let n = suffix.parse::<u64>().ok()?;
    Some((stem, n, suffix.len()))
}

fn candidate(stem: &str, n: u64, width: usize) -> String {
    format!("{stem}.{n:0width$}")
}

/// Whether `path` looks like a numbered segment at all.
///
/// Callers use this to skip discovery entirely for ordinary single-file images,
/// which matters on S3 where building a checker costs a network round trip.
pub fn has_numeric_suffix(path: &str) -> bool {
    split_numeric_suffix(path).is_some()
}

/// Every segment of the image `example` belongs to, in image order.
///
/// Numeric suffixes only, by deliberate choice: `.001`/`.002` covers FTK Imager
/// and the other forensic tools, and unlike `split(1)`'s `xaa`/`xab` it has an
/// unambiguous separator, so discovery cannot mistake an ordinary filename for a
/// segment. The digit width is taken from `example` and held fixed, so `.001`
/// never pairs with `.02`.
pub fn segment_paths<C: ExistsChecker>(
    example: &str,
    checker: &mut C,
) -> Result<Vec<String>, MissingSegment> {
    let Some((stem, num, width)) = split_numeric_suffix(example) else {
        // No suffix: an ordinary single-file image.
        return Ok(vec![example.to_string()]);
    };

    // Find the sequence's first number. Tools start at either 0 or 1, and the
    // caller may have named any segment, so probe both rather than assume.
    let first = if checker.exists(candidate(stem, 0, width)) {
        0
    } else if checker.exists(candidate(stem, 1, width)) {
        1
    } else {
        // Neither start exists, so `example` is not part of a sequence we can
        // reconstruct. Treat it as a lone file.
        return Ok(vec![example.to_string()]);
    };

    // Everything from the start up to the named segment must be present; a hole
    // there means we would otherwise serve an image with a chunk missing.
    for n in first..num {
        let path = candidate(stem, n, width);
        if !checker.exists(&path) {
            return Err(MissingSegment { path });
        }
    }

    // The caller asked for this exact path; it must exist. If it does not, we
    // would silently return a different image (the one before it), which is the
    // exact failure this module exists to prevent.
    if !checker.exists(example) {
        return Err(MissingSegment {
            path: example.to_string(),
        });
    }

    let mut paths = Vec::new();
    let mut n = first;
    loop {
        let path = candidate(stem, n, width);
        if !checker.exists(&path) {
            break;
        }
        paths.push(path);
        n += 1;
    }

    // Walking upward until a name is absent cannot, on its own, tell "the
    // sequence ends here" from "this one is missing and the rest follow". Left
    // there, `d.001 d.002 d.004` would open as a two-segment image and silently
    // drop the third -- the same short-image failure this whole feature exists to
    // prevent, moved one segment along. So look one past the end: if something is
    // there, we stopped at a hole, not at the end.
    let past = candidate(stem, n + 1, width);
    if checker.exists(&past) {
        return Err(MissingSegment {
            path: candidate(stem, n, width),
        });
    }

    Ok(paths)
}

#[cfg(test)]
mod test {
    use super::*;
    use std::collections::HashSet;

    /// Stands in for the filesystem so the naming rules can be tested without
    /// creating files.
    struct FakeFs(HashSet<String>);

    impl FakeFs {
        fn new(paths: &[&str]) -> Self {
            Self(paths.iter().map(|p| p.to_string()).collect())
        }
    }

    impl ExistsChecker for FakeFs {
        fn exists<T: AsRef<str>>(&mut self, path: T) -> bool {
            self.0.contains(path.as_ref())
        }
    }

    #[test]
    fn no_numeric_suffix_is_a_single_segment() {
        let mut fs = FakeFs::new(&["/img/disk.raw"]);
        assert_eq!(
            segment_paths("/img/disk.raw", &mut fs).unwrap(),
            vec!["/img/disk.raw"]
        );
    }

    #[test]
    fn walks_the_sequence_upward() {
        let mut fs = FakeFs::new(&["/img/d.001", "/img/d.002", "/img/d.003"]);
        assert_eq!(
            segment_paths("/img/d.001", &mut fs).unwrap(),
            vec!["/img/d.001", "/img/d.002", "/img/d.003"]
        );
    }

    /// Pointing at a later segment must still produce the whole image, not one
    /// that silently starts partway in.
    #[test]
    fn rewinds_to_the_first_segment() {
        let mut fs = FakeFs::new(&["/img/d.001", "/img/d.002", "/img/d.003"]);
        assert_eq!(
            segment_paths("/img/d.002", &mut fs).unwrap(),
            vec!["/img/d.001", "/img/d.002", "/img/d.003"]
        );
    }

    #[test]
    fn sequences_may_start_at_zero() {
        let mut fs = FakeFs::new(&["/img/d.000", "/img/d.001"]);
        assert_eq!(
            segment_paths("/img/d.001", &mut fs).unwrap(),
            vec!["/img/d.000", "/img/d.001"]
        );
    }

    /// Digit width is fixed by the example: `.001` never matches `.02` or `.0001`.
    #[test]
    fn digit_width_is_held_fixed() {
        let mut fs = FakeFs::new(&["/img/d.001", "/img/d.02"]);
        assert_eq!(
            segment_paths("/img/d.001", &mut fs).unwrap(),
            vec!["/img/d.001"]
        );
    }

    #[test]
    fn single_digit_suffixes_work() {
        let mut fs = FakeFs::new(&["/img/d.dd.1", "/img/d.dd.2"]);
        assert_eq!(
            segment_paths("/img/d.dd.1", &mut fs).unwrap(),
            vec!["/img/d.dd.1", "/img/d.dd.2"]
        );
    }

    /// The whole point of this work: a gap must be refused, never served as a
    /// short image.
    #[test]
    fn a_gap_below_the_example_is_an_error() {
        let mut fs = FakeFs::new(&["/img/d.001", "/img/d.003"]);
        let err = segment_paths("/img/d.003", &mut fs).unwrap_err();
        assert_eq!(err.path, "/img/d.002");
    }

    /// The mirror of the case above: the sequence must not quietly end at a
    /// hole that has segments beyond it.
    #[test]
    fn a_gap_above_the_example_is_an_error() {
        let mut fs = FakeFs::new(&["/img/d.001", "/img/d.002", "/img/d.004"]);
        let err = segment_paths("/img/d.001", &mut fs).unwrap_err();
        assert_eq!(err.path, "/img/d.003");
    }

    /// The lookahead is one segment wide, so a wider hole still reads as the end
    /// of the sequence. Pinned so the limit is a decision rather than a surprise.
    #[test]
    fn a_two_wide_gap_is_not_detected() {
        let mut fs = FakeFs::new(&["/img/d.001", "/img/d.005"]);
        assert_eq!(
            segment_paths("/img/d.001", &mut fs).unwrap(),
            vec!["/img/d.001"]
        );
    }

    #[test]
    fn a_lone_numbered_file_is_a_single_segment() {
        let mut fs = FakeFs::new(&["/img/d.001"]);
        assert_eq!(
            segment_paths("/img/d.001", &mut fs).unwrap(),
            vec!["/img/d.001"]
        );
    }

    /// Asking for a file that does not exist must not hand back a working
    /// reader over a different image. This prevents silent substitution.
    #[test]
    fn a_nonexistent_named_segment_is_an_error() {
        let mut fs = FakeFs::new(&["/img/d.001", "/img/d.002", "/img/d.003", "/img/d.004"]);
        let err = segment_paths("/img/d.005", &mut fs).unwrap_err();
        assert_eq!(err.path, "/img/d.005");
    }

    /// Distinguishing case for the lower-gap loop: without it, the walk would
    /// stop at d.002, the lookahead would check only d.003 (absent), and the
    /// function would return Ok(["d.001"]), silently dropping segments 002–005
    /// and the requested d.006 itself.
    #[test]
    fn a_wide_gap_below_the_example_is_an_error() {
        let mut fs = FakeFs::new(&["/img/d.001", "/img/d.006"]);
        let err = segment_paths("/img/d.006", &mut fs).unwrap_err();
        assert_eq!(err.path, "/img/d.002");
    }
}
