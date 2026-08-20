use imagesource::exists::{ExistsChecker, ExistsError};

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

/// Why discovery could not produce a segment list.
///
/// The two cases are deliberately separate. `Missing` is a fact about the
/// image; `Undetermined` is a fact about our ability to look, and folding it
/// into "absent" is what silently truncates an image on a throttled bucket.
#[derive(Debug, thiserror::Error)]
pub enum DiscoveryError {
    #[error("{0}")]
    Missing(#[from] MissingSegment),
    #[error("{0}")]
    Undetermined(#[from] ExistsError),
}

/// Fewest digits a suffix can have and still be read as a segment number.
///
/// Splitters emit fixed-width zero-padded numbering, so a real sequence has at
/// least two digits. A single one is far more likely to be a version or copy
/// marker, and the cost of guessing wrong there is the worst kind: `img.1` and
/// `img.2` as two unrelated images would open as one image serving `img.2`'s
/// bytes as the tail of `img.1`, with no error. No rule based on names alone
/// can tell that case from a genuine unpadded split, so unpadded numbering is
/// not treated as a sequence at all.
const MIN_SEGMENT_DIGITS: usize = 2;

/// Splits `path` into `(stem, number, digit width)` when it ends in `.` followed
/// by at least [`MIN_SEGMENT_DIGITS`] digits. `disk.001` -> `("disk", 1, 3)`;
/// `disk.raw` and `disk.1` -> `None`.
fn split_numeric_suffix(path: &str) -> Option<(&str, u64, usize)> {
    let (stem, suffix) = path.rsplit_once('.')?;
    if suffix.len() < MIN_SEGMENT_DIGITS || !suffix.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let n = suffix.parse::<u64>().ok()?;
    Some((stem, n, suffix.len()))
}

/// Whether `num` carries a leading zero when written to `width` digits.
///
/// This is the shape a fixed-width splitter gives every segment below the top
/// decade, and it is what separates `disk.004` from `backup.2024`: both are
/// four-or-fewer digits, but only one is padded, and only one is plausibly part
/// of a numbered sequence.
fn is_zero_padded(num: u64, width: usize) -> bool {
    match 10u64.checked_pow(width as u32 - 1) {
        Some(first_unpadded) => num < first_unpadded,
        // A width past u64's range; anything that parsed must have been padded.
        None => true,
    }
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
) -> Result<Vec<String>, DiscoveryError> {
    let Some((stem, num, width)) = split_numeric_suffix(example) else {
        // No suffix: an ordinary single-file image.
        return Ok(vec![example.to_string()]);
    };

    // Find the sequence's first number. Tools start at either 0 or 1, and the
    // caller may have named any segment, so probe both rather than assume.
    let first = if checker.exists(candidate(stem, 0, width))? {
        0
    } else if checker.exists(candidate(stem, 1, width))? {
        1
    } else {
        // No padded start at this width. Either `example` is a lone file that
        // happens to end in digits, or it is a sequence whose first segment was
        // lost -- and treating the second as a lone file hands back an image
        // with its start missing, the partition table itself.
        //
        // Only a padded name is worth that suspicion. `backup.2024` sits next to
        // `backup.2023` and `backup.2025` and would look exactly like a sequence
        // with a missing start, so an unpadded name is taken at face value as a
        // lone file.
        if !is_zero_padded(num, width) {
            return Ok(vec![example.to_string()]);
        }

        // A padded name with an immediate neighbor is a sequence missing its
        // start; lone files have no neighbors.
        //
        // `num` still needs `checked_add`: a suffix wider than u64's 20 digits
        // (`d.018446744073709551615`) parses to u64::MAX and counts as padded,
        // so it reaches here. There is no name above it to probe.
        let below = num > 0 && checker.exists(candidate(stem, num - 1, width))?;
        let above = match num.checked_add(1) {
            Some(next) => checker.exists(candidate(stem, next, width))?,
            None => false,
        };
        if below || above {
            // Name the file that is actually missing rather than assuming `.001`:
            // walk down to the lowest segment present and report the one below
            // it. A sequence triaged out of a larger set starts partway up, and
            // pointing the operator at `.001` sends them after a file that was
            // never part of this image.
            let mut lowest = num;
            while lowest > 0 && checker.exists(candidate(stem, lowest - 1, width))? {
                lowest -= 1;
            }
            return Err(MissingSegment {
                path: candidate(stem, lowest.saturating_sub(1), width),
            }
            .into());
        }
        return Ok(vec![example.to_string()]);
    };

    // One walk upward from the start, probing each name exactly once. `n` ends
    // on the first name that is not there.
    let mut paths = Vec::new();
    let mut n = first;
    loop {
        let path = candidate(stem, n, width);
        if !checker.exists(&path)? {
            break;
        }
        paths.push(path);
        n += 1;
    }

    // The named segment has to be inside the run we just walked, `first..n`.
    // Outside it in either direction we would hand back a working reader over a
    // different image than the one asked for -- the silent substitution this
    // module exists to prevent.
    //
    // Below the start: `first` is 1 and the caller named `.000`, which the start
    // probe already found absent. Above the end: the walk stopped at a hole
    // (`d.001 d.006`, asked for d.006) or the caller named a segment past the
    // end (`d.001..d.004`, asked for d.005). Name whichever one is missing.
    if num < first {
        return Err(MissingSegment {
            path: candidate(stem, num, width),
        }
        .into());
    }
    if num >= n {
        return Err(MissingSegment {
            path: candidate(stem, n, width),
        }
        .into());
    }

    // Walking upward until a name is absent cannot, on its own, tell "the
    // sequence ends here" from "this one is missing and the rest follow". Left
    // there, `d.001 d.002 d.004` would open as a two-segment image and silently
    // drop the third -- the same short-image failure this whole feature exists to
    // prevent, moved one segment along. So look one past the end: if something is
    // there, we stopped at a hole, not at the end.
    //
    // `n + 1` cannot overflow the way `num + 1` above can: `num` comes from the
    // filename, but `n` only got here by counting files that actually exist.
    let past = candidate(stem, n + 1, width);
    if checker.exists(&past)? {
        return Err(MissingSegment {
            path: candidate(stem, n, width),
        }
        .into());
    }

    Ok(paths)
}

#[cfg(test)]
mod test {
    use super::*;
    use std::collections::HashSet;

    /// Stands in for the filesystem so the naming rules can be tested without
    /// creating files. `undetermined` names paths the checker cannot answer for,
    /// standing in for a throttled HEAD or an unreadable directory.
    struct FakeFs {
        present: HashSet<String>,
        undetermined: HashSet<String>,
        /// Every probe, in order. On S3 each one is a network round trip, so the
        /// count is a cost the tests get to assert on.
        probes: Vec<String>,
    }

    impl FakeFs {
        fn new(paths: &[&str]) -> Self {
            Self {
                present: paths.iter().map(|p| p.to_string()).collect(),
                undetermined: HashSet::new(),
                probes: Vec::new(),
            }
        }

        fn with_undetermined(paths: &[&str], undetermined: &[&str]) -> Self {
            Self {
                undetermined: undetermined.iter().map(|p| p.to_string()).collect(),
                ..Self::new(paths)
            }
        }
    }

    impl ExistsChecker for FakeFs {
        fn exists<T: AsRef<str>>(&mut self, path: T) -> Result<bool, ExistsError> {
            let path = path.as_ref();
            self.probes.push(path.to_string());
            if self.undetermined.contains(path) {
                return Err(ExistsError::new(
                    path,
                    std::io::Error::other("HEAD returned HTTP 503"),
                ));
            }
            Ok(self.present.contains(path))
        }
    }

    /// Asserts the error is a missing segment and yields its path, so the tests
    /// below cannot pass on an `Undetermined` that happens to name the right file.
    fn missing(e: DiscoveryError) -> MissingSegment {
        match e {
            DiscoveryError::Missing(m) => m,
            DiscoveryError::Undetermined(e) => panic!("expected a missing segment, got {e}"),
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

    /// Single-digit numbering is not a sequence. `img.1` and `img.2` are far
    /// more often two unrelated images than one split one, and concatenating
    /// them serves `img.2`'s bytes as the tail of `img.1` with no error at all.
    /// Nothing in the names can tell the two apart, so neither is treated as a
    /// sequence and each opens as itself.
    #[test]
    fn single_digit_suffixes_are_lone_files() {
        let mut fs = FakeFs::new(&["/img/d.dd.1", "/img/d.dd.2"]);
        assert_eq!(
            segment_paths("/img/d.dd.1", &mut fs).unwrap(),
            vec!["/img/d.dd.1"]
        );
    }

    /// Dated backups sitting next to each other look exactly like a sequence
    /// with a missing start -- consecutive numbers, same width. Padding is what
    /// separates them: a splitter writes `.0024`, a date is just `2024`.
    #[test]
    fn unpadded_neighbors_are_lone_files_not_a_broken_sequence() {
        let mut fs = FakeFs::new(&["/img/backup.2023", "/img/backup.2024", "/img/backup.2025"]);
        assert_eq!(
            segment_paths("/img/backup.2024", &mut fs).unwrap(),
            vec!["/img/backup.2024"]
        );
    }

    /// An unpadded run is not a sequence from any of its members, so naming a
    /// later one cannot produce a spurious gap. Previously `d.20` of `d.1..d.20`
    /// probed `d.00`/`d.01`, found `d.11` as a neighbor, and failed naming
    /// `d.01` -- a file that never existed.
    #[test]
    fn an_unpadded_run_is_not_a_sequence_from_any_member() {
        let files: Vec<String> = (1..=20).map(|i| format!("/img/d.{i}")).collect();
        let refs: Vec<&str> = files.iter().map(String::as_str).collect();
        let mut fs = FakeFs::new(&refs);
        for named in ["/img/d.1", "/img/d.10", "/img/d.20"] {
            assert_eq!(
                segment_paths(named, &mut fs).unwrap(),
                vec![named.to_string()],
                "{named}"
            );
        }
    }

    /// A padded sequence still works from any member, including the last, which
    /// is what the README tells operators to name.
    #[test]
    fn a_padded_sequence_opens_from_any_member() {
        let files: Vec<String> = (1..=20).map(|i| format!("/img/d.{i:03}")).collect();
        let refs: Vec<&str> = files.iter().map(String::as_str).collect();
        for named in ["/img/d.001", "/img/d.010", "/img/d.020"] {
            let mut fs = FakeFs::new(&refs);
            assert_eq!(segment_paths(named, &mut fs).unwrap(), files, "{named}");
        }
    }

    /// A filename is not a promise that the number in it is sane. `u64::MAX`
    /// parses, and probing the neighbor above it used to compute `num + 1` --
    /// a panic in debug, a wrap to `d.000...000` in release. Reachable from
    /// nothing more exotic than a file sitting in a directory being triaged.
    #[test]
    fn a_segment_number_at_the_top_of_u64_does_not_overflow() {
        let name = format!("/img/d.{}", u64::MAX);
        let mut fs = FakeFs::new(&[&name]);
        assert_eq!(segment_paths(&name, &mut fs).unwrap(), vec![name.clone()]);
    }

    /// u64::MAX is unpadded at its natural 20 digits, so it is a lone file and
    /// the neighbor probes -- where the overflow lived -- are never reached.
    #[test]
    fn a_top_of_u64_segment_with_a_neighbor_below_is_a_lone_file() {
        let below = format!("/img/d.{}", u64::MAX - 1);
        let name = format!("/img/d.{}", u64::MAX);
        let mut fs = FakeFs::new(&[&below, &name]);
        assert_eq!(segment_paths(&name, &mut fs).unwrap(), vec![name.clone()]);
    }

    /// Padding it out past u64's 20 digits does reach the neighbor probes, so
    /// `num + 1` still has to be guarded there.
    #[test]
    fn a_padded_over_wide_segment_number_does_not_overflow() {
        let name = format!("/img/d.0{}", u64::MAX);
        let below = format!("/img/d.0{}", u64::MAX - 1);
        let mut fs = FakeFs::new(&[&below, &name]);
        // Reaches the neighbor probes: `below` exists, so this is a sequence
        // with a missing start rather than a lone file.
        assert!(segment_paths(&name, &mut fs).is_err());
    }

    /// The whole point of this work: a gap must be refused, never served as a
    /// short image.
    #[test]
    fn a_gap_below_the_example_is_an_error() {
        let mut fs = FakeFs::new(&["/img/d.001", "/img/d.003"]);
        let err = missing(segment_paths("/img/d.003", &mut fs).unwrap_err());
        assert_eq!(err.path, "/img/d.002");
    }

    /// The mirror of the case above: the sequence must not quietly end at a
    /// hole that has segments beyond it.
    #[test]
    fn a_gap_above_the_example_is_an_error() {
        let mut fs = FakeFs::new(&["/img/d.001", "/img/d.002", "/img/d.004"]);
        let err = missing(segment_paths("/img/d.001", &mut fs).unwrap_err());
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

    /// A sequence whose first segment is absent must not open as a short image
    /// starting partway in. Detected from a neighbor above the named segment.
    #[test]
    fn a_missing_sequence_start_is_an_error() {
        let mut fs = FakeFs::new(&["/img/d.002", "/img/d.003", "/img/d.004"]);
        let err = missing(segment_paths("/img/d.002", &mut fs).unwrap_err());
        assert_eq!(err.path, "/img/d.001");
    }

    /// The error names the file actually missing, not always `.001`. Segments
    /// triaged out of a larger set start partway up, and pointing the operator
    /// at `.001` sends them after a file that was never part of this image.
    #[test]
    fn a_missing_start_names_the_gap_below_the_lowest_segment_present() {
        let mut fs = FakeFs::new(&["/img/s.004", "/img/s.005", "/img/s.006"]);
        let err = missing(segment_paths("/img/s.004", &mut fs).unwrap_err());
        assert_eq!(err.path, "/img/s.003");
    }

    /// The same, spotted from the far end: the only neighbor is below.
    #[test]
    fn a_missing_start_is_caught_from_the_last_segment() {
        let mut fs = FakeFs::new(&["/img/d.002", "/img/d.003"]);
        let err = missing(segment_paths("/img/d.003", &mut fs).unwrap_err());
        assert_eq!(err.path, "/img/d.001");
    }

    /// The mirror of `a_nonexistent_named_segment_is_an_error`, below the run
    /// rather than above it: the sequence starts at `.001`, and the caller asked
    /// for a `.000` that is not there. Returning the `.001`-onward image would be
    /// silent substitution -- a working reader over something other than what was
    /// named. The range check has to bound the run at both ends, not just the top.
    #[test]
    fn a_named_segment_below_the_sequence_start_is_an_error() {
        let mut fs = FakeFs::new(&["/img/d.001", "/img/d.002"]);
        let err = missing(segment_paths("/img/d.000", &mut fs).unwrap_err());
        assert_eq!(err.path, "/img/d.000");
    }

    /// Asking for a file that does not exist must not hand back a working
    /// reader over a different image. This prevents silent substitution.
    #[test]
    fn a_nonexistent_named_segment_is_an_error() {
        let mut fs = FakeFs::new(&["/img/d.001", "/img/d.002", "/img/d.003", "/img/d.004"]);
        let err = missing(segment_paths("/img/d.005", &mut fs).unwrap_err());
        assert_eq!(err.path, "/img/d.005");
    }

    /// Distinguishing case for the range check: the walk stops at d.002, and
    /// without noticing that the requested d.006 is above that, the function
    /// would return Ok(["d.001"]) -- silently dropping segments 002-005 and the
    /// requested d.006 itself.
    #[test]
    fn a_wide_gap_below_the_example_is_an_error() {
        let mut fs = FakeFs::new(&["/img/d.001", "/img/d.006"]);
        let err = missing(segment_paths("/img/d.006", &mut fs).unwrap_err());
        assert_eq!(err.path, "/img/d.002");
    }

    /// Discovery probes each name once. It used to walk `first..num` and then
    /// walk again from `first`, so every segment below the named one cost two
    /// probes -- on S3 that is two HEAD round trips each, and the caller naming
    /// the last segment of a long sequence is the ordinary case, not a corner.
    #[test]
    fn each_name_is_probed_once() {
        let files: Vec<String> = (1..=20).map(|i| format!("/img/d.{i:03}")).collect();
        let refs: Vec<&str> = files.iter().map(String::as_str).collect();

        let mut fs = FakeFs::new(&refs);
        assert_eq!(segment_paths("/img/d.020", &mut fs).unwrap(), files);

        // Only d.001 is probed twice, and only because the start probe and the
        // walk both begin there. Every other name is asked about once.
        let repeated: Vec<&String> = files
            .iter()
            .filter(|f| fs.probes.iter().filter(|p| p == f).count() > 1)
            .collect();
        assert_eq!(repeated, vec!["/img/d.001"], "{:?}", fs.probes);

        // d.000 + d.001 to find the start, d.001..=d.021 walking, d.022 for the
        // lookahead. The old double walk cost 44.
        assert_eq!(fs.probes.len(), 24, "{:?}", fs.probes);
    }

    /// A checker that cannot answer must stop the open. Reading "I could not
    /// tell" as "absent" ends the walk early, and the image opens short with no
    /// error at all -- the failure the whole module is built to prevent, now
    /// triggered by an S3 throttle rather than a real gap.
    #[test]
    fn an_undetermined_probe_mid_sequence_is_not_the_end_of_the_sequence() {
        let mut fs =
            FakeFs::with_undetermined(&["/img/d.001", "/img/d.002", "/img/d.003"], &["/img/d.003"]);
        match segment_paths("/img/d.001", &mut fs).unwrap_err() {
            DiscoveryError::Undetermined(e) => assert!(e.to_string().contains("/img/d.003"), "{e}"),
            e => panic!("expected Undetermined, got {e}"),
        }
    }

    /// The same at the very first probe. This one used to be the worst case: a
    /// failure here made both start candidates and both neighbors look absent,
    /// so a split image fell through to the lone-file path and opened as one
    /// segment of N.
    #[test]
    fn an_undetermined_start_probe_does_not_degrade_to_a_lone_file() {
        let mut fs =
            FakeFs::with_undetermined(&["/img/d.001", "/img/d.002"], &["/img/d.000", "/img/d.001"]);
        match segment_paths("/img/d.002", &mut fs).unwrap_err() {
            DiscoveryError::Undetermined(e) => assert!(e.to_string().contains("/img/d.000"), "{e}"),
            e => panic!("expected Undetermined, got {e}"),
        }
    }

    /// The lookahead is a probe like any other, and it decides whether the
    /// sequence really ended. An unanswerable one must not be read as "ended".
    #[test]
    fn an_undetermined_lookahead_is_not_the_end_of_the_sequence() {
        let mut fs = FakeFs::with_undetermined(&["/img/d.001", "/img/d.002"], &["/img/d.004"]);
        match segment_paths("/img/d.001", &mut fs).unwrap_err() {
            DiscoveryError::Undetermined(e) => assert!(e.to_string().contains("/img/d.004"), "{e}"),
            e => panic!("expected Undetermined, got {e}"),
        }
    }
}
