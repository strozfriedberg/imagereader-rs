use std::sync::{Arc, RwLock};

use crate::bytessource::BytesSource;

type Source = Arc<dyn BytesSource + Send + Sync>;

/// The sources backing a cache, indexed by segment/extent number.
///
/// Sources are registered by index after the cache is built, and a reader can
/// leave a gap -- `vmdk` increments its index in two places while walking an
/// extent chain, so an index can be skipped if the metadata is malformed. A gap
/// is `None` and reading one is an error.
///
/// This used to be a `Vec<Arc<dyn BytesSource>>` backfilled with a
/// `PlaceholderSource` whose every method was `unreachable!()`, which turned a
/// recoverable "that segment never opened" into a panic -- inside a cache read,
/// reachable from a malformed image.
#[derive(Default)]
pub struct SourceSlots {
    slots: RwLock<Vec<Option<Source>>>,
}

pub fn unregistered(idx: usize) -> std::io::Error {
    std::io::Error::new(
        std::io::ErrorKind::InvalidData,
        format!("source {idx} was never registered"),
    )
}

impl SourceSlots {
    /// The source at `idx`, or an error if the index is out of range or its
    /// slot was never filled in.
    pub fn get(&self, idx: usize) -> Result<Source, std::io::Error> {
        self.slots
            .read()
            .expect("sources lock poisoned")
            .get(idx)
            .cloned()
            .flatten()
            .ok_or_else(|| unregistered(idx))
    }

    /// Register `src` at `idx`, growing the table (with gaps as `None`) if needed.
    pub fn set(&self, idx: usize, src: Box<dyn BytesSource + Send + Sync>) {
        let mut slots = self.slots.write().expect("sources lock poisoned");
        if slots.len() <= idx {
            slots.resize_with(idx + 1, || None);
        }
        slots[idx] = Some(Arc::from(src));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::filesource::FileSource;

    fn a_source() -> Box<dyn BytesSource + Send + Sync> {
        Box::new(FileSource {
            path: "/nonexistent".into(),
            len: 42,
        })
    }

    #[test]
    fn reads_a_registered_source() {
        let slots = SourceSlots::default();
        slots.set(0, a_source());
        assert_eq!(slots.get(0).unwrap().end(), 42);
    }

    #[test]
    fn an_out_of_range_index_is_an_error() {
        let slots = SourceSlots::default();
        assert!(slots.get(7).is_err());
    }

    /// Registering index 1 leaves index 0 a gap. Reading it must be an error --
    /// this is the case the old PlaceholderSource answered with `unreachable!()`.
    #[test]
    fn a_gap_is_an_error_not_a_panic() {
        let slots = SourceSlots::default();
        slots.set(1, a_source());

        let err = slots.get(0).err().expect("a gap must be an error");
        assert_eq!(err.kind(), std::io::ErrorKind::InvalidData);
        assert!(slots.get(1).is_ok());
    }
}
