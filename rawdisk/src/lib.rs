pub mod rawdisk_reader;
pub mod seg_path;
pub mod spans;

#[cfg(feature = "capi")]
pub mod capi;

#[cfg(test)]
mod test_data;

#[cfg(test)]
mod test_helper;

pub use imagesource::{IoLog, ReadTimer, ReadTrace, chunk_cache_label, init_tracing};

#[cfg(test)]
mod test {
    use crate::{rawdisk_reader::RawdiskReader, test_data::*, test_helper::do_hash};

    #[track_caller]
    fn assert_eq_test_data(exp: &TestData) {
        let reader = RawdiskReader::open(exp.image_path).unwrap();
        let image_size = reader.image_size;

        let sha1 = do_hash(
            |offset, buf: &mut [u8]| {
                let buf_len = buf.len();
                reader.read_at_offset(offset, &mut buf[..buf_len]).unwrap()
            },
            image_size,
            false,
        );

        let act = TestData {
            image_path: exp.image_path,
            image_size: reader.image_size,
            sha1: &sha1,
        };

        assert_eq!(&act, exp);
    }

    #[test]
    fn test_patterned_4mib_raw() {
        assert_eq_test_data(&PATTERNED_4MIB);
    }

    #[test]
    fn test_unaligned_3mib_512b_raw() {
        assert_eq_test_data(&UNALIGNED_3MIB_512B);
    }
}
