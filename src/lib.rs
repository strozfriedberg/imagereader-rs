pub mod e01_reader;
mod error;
mod generated;
pub mod hasher;
mod sec_read;
mod seg_path;
mod segment;

#[cfg(test)]
mod test {
    use crate::{
        e01_reader::E01Reader,
        hasher::{HashType, MultiHasher}
    };

    use hex;
    use rand::Rng;
    use std::collections::HashMap;

    #[track_caller]
    fn do_hash(
        reader: &E01Reader,
        random_buf_size: bool
    ) -> HashMap<HashType, String>
    {
        let mut hasher = MultiHasher::from([
            HashType::MD5,
            HashType::SHA1,
            HashType::SHA256
        ]);

        let mut buf: Vec<u8> = vec![0; 1048576];
        let mut offset = 0;

        while offset < reader.total_size() {
            let buf_size = if random_buf_size {
                rand::rng().random_range(0..buf.len())
            }
            else {
                buf.len()
            };

            let read = reader
                .read_at_offset(offset, &mut buf[..buf_size])
                .unwrap();
            if read == 0 {
                break;
            }

            hasher.update(&buf[..read]);

            offset += read;
        }

        hasher.finalize()
            .into_iter()
            .map(|(k, v)| (k, hex::encode(v)))
            .collect()
    }

    #[track_caller]
    fn assert_hash_all(
        td: &TestData,
        ignore_checksums: bool
    ) {
        let reader = E01Reader::open_glob(
            td.path,
            ignore_checksums
        ).unwrap();

        let stored_md5 = reader.get_stored_md5().map(hex::encode);
        let stored_sha1 = reader.get_stored_sha1().map(hex::encode);

        let hashes = do_hash(&reader, false);

        assert_eq!(hashes.get(&HashType::MD5), stored_md5.as_ref());
        assert_eq!(hashes.get(&HashType::SHA1), stored_sha1.as_ref());

        assert_eq!(hashes.get(&HashType::MD5).unwrap(), td.md5);
        assert_eq!(hashes.get(&HashType::SHA1).unwrap(), td.sha1);
        assert_eq!(hashes.get(&HashType::SHA256).unwrap(), td.sha256);
    }

    struct TestData {
        pub path: &'static str,
        pub md5: &'static str,
        pub sha1: &'static str,
        pub sha256: &'static str
    }

    const IMAGE_E01: TestData = TestData {
        path: "data/image.E01",
        md5: "28035e42858e28326c23732e6234bcf8",
        sha1: "e5c6c296485b1146fead7ad552e1c3ccfc00bfab",
        sha256: "cab8049f5fba42e06609c9d0678eb9fff7fcb50afc6c9b531ee6216bbe40a827"
    };

    const MIMAGE_E01: TestData = TestData {
        path: "data/mimage.E01",
        md5: "5be32cdd1b96eac4d4a41d13234ee599",
        sha1: "f8677bd8a38a12476ae655a9f9f5336c287603f7",
        sha256: "bc730943b2247e11b18caf272b1e78289267864962751549b1722752bf1e2e3d"
    };

    #[test]
    fn test_image_e01_checksums() {
        assert_hash_all(&IMAGE_E01, false);
    }

/*
    #[test]
    fn test_image_e01_no_checksums() {
        assert_hash_all(&IMAGE_E01, true);
    }
*/

    #[test]
    fn test_mimage_e01_checksums() {
        assert_hash_all(&MIMAGE_E01, false);
    }

/*
    #[test]
    fn test_mimage_e01_no_checksums() {
        assert_hash_all(&MIMAGE_E01, true);
    }
*/

/*
    const DADEMURPHY_E01: TestData = TestData {
        path: "/home/juckelman/Downloads/dademurphy.E01",
        md5: "caadd3db26d633249fcf9143d67d69bd",
        sha1: "109a68fc6921ea3f30aa5718177a435222b4fd15",
        sha256: "6a3720e277f54e9038b8faa5266aaa30cc5912511fbbac7256f570fa46e7060c"
    };

    #[test]
    fn test_dademurphy_e01() {
        assert_hash_all(DADEMURPHY_E01, false);
    }

    const NFURY_E01: TestData = TestData {
        path: "/home/juckelman/Downloads/nfury/win7-64-nfury-c-drive.E01",
        md5: "a98416e60bb81f57cb99125ec41bfe4c",
        sha1: "829553fd43bbd6d69c85d8285b83410ac679b066",
        sha256: "03e762e3f2732f30dd83675469129cb0a7a8e225dcbecdad1829ab4600277763"
    };

    #[test]
    fn test_nfury_e01() {
        assert_hash_all(NFURY_E01, false);
    }
*/
}
