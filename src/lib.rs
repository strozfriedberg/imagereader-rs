pub mod e01_reader;
mod generated;

#[allow(unused)]
mod test {
    use crate::e01_reader::E01Reader;

    use rand::Rng;
    use sha2::{Digest, Sha256};

    #[track_caller]
    fn do_hash(e01_path: &str, random_buf_size: bool) -> String /*hash*/ {
        let e01_reader = E01Reader::open(e01_path, false).unwrap();

        let mut hasher = Sha256::new();
        let mut buf: Vec<u8> = vec![0; 1048576];
        let mut offset = 0;

        while offset < e01_reader.total_size() {
            let buf_size = if random_buf_size {
                rand::rng().random_range(0..buf.len())
            }
            else {
                buf.len()
            };

            let read = e01_reader.read_at_offset(offset, &mut buf[..buf_size]).unwrap();
            if read == 0 {
                break;
            }

            hasher.update(&buf[..read]);

            offset += read;
        }

        let result = hasher.finalize();
        format!("{:X}", result)
    }

    #[track_caller]
    fn assert_hash_both(image_path: &str, expected_hash: &str) {
        assert_eq!(do_hash(image_path, false), expected_hash);
        assert_eq!(do_hash(image_path, true), expected_hash);
    }

    #[test]
    fn test_image_e01() {
        assert_hash_both(
            "data/image.E01",
            "CAB8049F5FBA42E06609C9D0678EB9FFF7FCB50AFC6C9B531EE6216BBE40A827"
        );
    }

    #[test]
    fn test_mimage_e01() {
        assert_hash_both(
            "data/mimage.E01",
            "BC730943B2247E11B18CAF272B1E78289267864962751549B1722752BF1E2E3D"
        );
    }

/*
    #[test]
    fn test_dademurphy_e01() {
        assert_hash_both(
            "/home/juckelman/Downloads/dademurphy.E01"
        );
    }

    #[test]
    fn test_nfury_e01() {
        do_hash_both("/mnt/c/evidence/nfury/win7-64-nfury-c-drive.E01");
    }
*/
}
