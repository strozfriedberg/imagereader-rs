/// Maps an image offset to the segment holding it.
///
/// A split raw image is a plain concatenation: segment 0 supplies the first
/// bytes, segment 1 the next, and so on. Nothing in the format records the
/// boundaries, so they come from the segments' own lengths, which means they
/// are rarely uniform -- the last segment is short, and some tools emit a short
/// one in the middle. The map therefore stores cumulative starts and binary
/// searches them rather than dividing by a stride.
#[derive(Debug, Clone)]
pub struct SegmentMap {
    /// Cumulative start offset of each segment. `starts[i]` is the image offset
    /// of segment `i`'s first byte. Always the same length as `lengths`.
    starts: Vec<u64>,
    lengths: Vec<u64>,
    image_size: u64,
}

impl SegmentMap {
    /// # Invariant
    ///
    /// No segment may be zero-length unless it is the only one. A zero-length
    /// segment shares its start offset with the next, which would make `starts`
    /// non-monotonic and `locate`'s binary search ambiguous. `RawdiskReader`
    /// enforces this at open by refusing an empty segment in a multi-segment
    /// image -- it has to refuse anyway, since skipping one would shift every
    /// later segment down and serve wrong bytes for the tail of the image.
    ///
    /// A lone empty file is allowed: it gives `image_size == 0`, so `locate`
    /// returns `None` for every offset and never inspects the length.
    pub fn new(lengths: Vec<u64>) -> Self {
        debug_assert!(
            lengths.len() < 2 || lengths.iter().all(|&l| l > 0),
            "zero-length segment in a multi-segment map: {lengths:?}"
        );
        let mut starts = Vec::with_capacity(lengths.len());
        let mut acc = 0u64;
        for len in &lengths {
            starts.push(acc);
            // Saturating only as a formality: the total is a sum of file sizes,
            // so reaching u64 would take 16 exabytes of segments.
            acc = acc.saturating_add(*len);
        }
        Self {
            starts,
            lengths,
            image_size: acc,
        }
    }

    pub fn image_size(&self) -> u64 {
        self.image_size
    }

    /// `(segment index, offset within that segment, bytes left in that segment)`.
    /// `None` once `offset` reaches the end of the image.
    ///
    /// The remaining count is never zero for an offset inside the image, which
    /// is what lets the caller's read loop make progress.
    pub fn locate(&self, offset: u64) -> Option<(usize, u64, u64)> {
        if offset >= self.image_size {
            return None;
        }
        // Reaching here means the map has a segment holding this byte, so it is
        // not the lone-empty-file case and every length is non-zero. `starts` is
        // therefore strictly increasing and the search is unambiguous: `Ok(i)`
        // is exactly a segment start, `Err(i)` falls inside segment `i - 1`.
        // `Err(0)` cannot happen, because `starts[0]` is 0 and no offset sorts
        // below it.
        let i = match self.starts.binary_search(&offset) {
            Ok(i) => i,
            Err(i) => i - 1,
        };
        let within = offset - self.starts[i];
        Some((i, within, self.lengths[i] - within))
    }
}

#[cfg(test)]
mod test {
    use super::*;

    /// Three 100-byte segments. Every lookup has to name the right segment, the
    /// right offset inside it, and how much of that segment is left -- the read
    /// loop uses the third value to decide how much it may copy before moving on.
    #[test]
    fn locate_maps_offsets_to_segments() {
        let m = SegmentMap::new(vec![100, 100, 100]);
        assert_eq!(m.image_size(), 300);

        assert_eq!(m.locate(0), Some((0, 0, 100)));
        assert_eq!(m.locate(99), Some((0, 99, 1)));
        // A boundary offset belongs to the segment that starts there, not the
        // one that ends there.
        assert_eq!(m.locate(100), Some((1, 0, 100)));
        assert_eq!(m.locate(150), Some((1, 50, 50)));
        assert_eq!(m.locate(299), Some((2, 99, 1)));
    }

    #[test]
    fn locate_past_the_end_is_none() {
        let m = SegmentMap::new(vec![100, 100]);
        assert_eq!(m.locate(200), None);
        assert_eq!(m.locate(u64::MAX), None);
    }

    /// Segments are rarely equal: the last one is short, and some tools emit a
    /// short segment in the middle. The map must not assume a uniform stride.
    #[test]
    fn locate_handles_uneven_segments() {
        let m = SegmentMap::new(vec![10, 1, 1000]);
        assert_eq!(m.image_size(), 1011);
        assert_eq!(m.locate(9), Some((0, 9, 1)));
        assert_eq!(m.locate(10), Some((1, 0, 1)));
        assert_eq!(m.locate(11), Some((2, 0, 1000)));
    }

    /// The one legitimate zero length: a lone empty file. `image_size` is 0, so
    /// `locate` never reaches the length and the map stays consistent.
    #[test]
    fn a_lone_empty_segment_is_a_zero_sized_image() {
        let m = SegmentMap::new(vec![0]);
        assert_eq!(m.image_size(), 0);
        assert_eq!(m.locate(0), None);
    }

    /// A zero-length segment among others breaks the map's invariant: it shares
    /// a start with its neighbour, which makes the binary search ambiguous.
    /// `RawdiskReader` refuses one at open, so reaching here is a bug, and the
    /// map says so rather than quietly returning a zero-byte span.
    #[cfg(debug_assertions)]
    #[test]
    #[should_panic(expected = "zero-length segment in a multi-segment map")]
    fn an_empty_segment_among_others_is_a_contract_violation() {
        SegmentMap::new(vec![10, 0, 10]);
    }

    #[test]
    fn single_segment_behaves_like_the_old_identity_mapping() {
        let m = SegmentMap::new(vec![4096]);
        assert_eq!(m.image_size(), 4096);
        assert_eq!(m.locate(0), Some((0, 0, 4096)));
        assert_eq!(m.locate(4095), Some((0, 4095, 1)));
        assert_eq!(m.locate(4096), None);
    }

    #[test]
    fn empty_map_is_zero_sized() {
        let m = SegmentMap::new(vec![]);
        assert_eq!(m.image_size(), 0);
        assert_eq!(m.locate(0), None);
    }
}
