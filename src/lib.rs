pub mod e01_reader;

#[cfg(feature = "capi")]
pub mod capi;

#[cfg(test)]
mod test_data;

#[cfg(test)]
mod test_helper;

mod error;
mod generated;
pub mod hasher;
mod readworker;
mod sec_read;
mod seg_path;
mod segment;

#[cfg(test)]
mod test {
    use crate::{
        e01_reader::{CorruptChunkPolicy, CorruptSectionPolicy, E01Reader, E01ReaderOptions},
        hasher::HashType,
        test_data::*,
        test_helper::do_hash
    };

    #[track_caller]
    fn assert_eq_test_data(exp: &TestData, options: &E01ReaderOptions) {
        let mut reader = E01Reader::open_glob(
            exp.segment_paths[0],
            options
        ).unwrap();

        let image_size = reader.image_size;

        let hashes = do_hash(
            |offset, buf: &mut [u8]| {
                let buf_len = buf.len();
                reader.read_at_offset(offset, &mut buf[..buf_len])
                    .unwrap()
            },
            image_size,
            false
        );

        let stored_md5 = reader.stored_md5.map(hex::encode);
        let stored_sha1 = reader.stored_sha1.map(hex::encode);

        let segment_paths = reader.segment_paths
            .iter()
            .map(|p| p.to_str())
            .collect::<Option<Vec<_>>>()
            .unwrap();

        let act = TestData {
            segment_paths: &segment_paths[..],
            chunk_size: reader.chunk_size,
            chunk_count: reader.chunk_count,
            sector_size: reader.sector_size,
            sector_count: reader.sector_count,
            image_size: reader.image_size,
            stored_md5: stored_md5.as_deref(),
            stored_sha1: stored_sha1.as_deref(),
            md5: hashes.get(&HashType::MD5).map(String::as_str),
            sha1: hashes.get(&HashType::SHA1).map(String::as_str),
            sha256: hashes.get(&HashType::SHA256).map(String::as_str)
        };

        assert_eq!(&act, exp);
    }

    const ERROR_ERROR: E01ReaderOptions = E01ReaderOptions {
        corrupt_section_policy: CorruptSectionPolicy::Error,
        corrupt_chunk_policy: CorruptChunkPolicy::Error
    };

    const ERROR_ZERO: E01ReaderOptions = E01ReaderOptions {
        corrupt_section_policy: CorruptSectionPolicy::Error,
        corrupt_chunk_policy: CorruptChunkPolicy::Zero
    };

    #[test]
    fn test_image_e01() {
        assert_eq_test_data(&IMAGE_E01, &ERROR_ERROR);
    }

    #[test]
    fn test_image_e01_zero_bad_chunks() {
        assert_eq_test_data(&IMAGE_E01, &ERROR_ZERO);
    }

    #[test]
    fn test_mimage_e01() {
        assert_eq_test_data(&MIMAGE_E01, &ERROR_ERROR);
    }

    #[test]
    fn test_mimage_e01_zero_bad_chunks() {
        assert_eq_test_data(&MIMAGE_E01, &ERROR_ZERO);
    }

    #[test]
    #[should_panic]
    fn test_bad_chunk_e01() {
        assert_eq_test_data(&BAD_CHUNK_E01, &ERROR_ERROR);
    }

    #[test]
    fn test_bad_chunk_e01_zero_bad_chunks() {
        assert_eq_test_data(&BAD_CHUNK_E01_ZEROED, &ERROR_ZERO);
    }

/*
    const DADEMURPHY_E01: TestData = TestData {
        path: "/home/juckelman/Downloads/dademurphy.E01",
        md5: "caadd3db26d633249fcf9143d67d69bd",
        sha1: "109a68fc6921ea3f30aa5718177a435222b4fd15",
        sha256: "6a3720e277f54e9038b8faa5266aaa30cc5912511fbbac7256f570fa46e7060c"
    };

    #[test]
    fn test_dademurphy_e01() {
        assert_eq_test_data(DADEMURPHY_E01, &ERROR_ERROR);
    }

    const NFURY_E01: TestData = TestData {
        path: "/home/juckelman/Downloads/nfury/win7-64-nfury-c-drive.E01",
        md5: "a98416e60bb81f57cb99125ec41bfe4c",
        sha1: "829553fd43bbd6d69c85d8285b83410ac679b066",
        sha256: "03e762e3f2732f30dd83675469129cb0a7a8e225dcbecdad1829ab4600277763"
    };

    #[test]
    fn test_nfury_e01() {
        assert_eq_test_data(NFURY_E01, &ERROR_ERROR);
    }
*/
}
