pub mod e01_reader;
mod generated;
mod seg_path;
mod seg_read;

#[cfg(test)]
mod test {
    use crate::e01_reader::E01Reader;

    use hex;
    use md5::digest::DynDigest;
    use md5::Md5;
    use rand::Rng;
    use sha1::Sha1;
    use sha2::{Digest, Sha256};

    #[track_caller]
    fn do_hash(
        reader: &E01Reader,
        random_buf_size: bool
    ) -> (String, String, String)
    {
        let mut hashers: Vec<Box<dyn DynDigest>> = vec![
            Box::new(Md5::new()),
            Box::new(Sha1::new()),
            Box::new(Sha256::new())
        ];

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

            hashers.iter_mut().for_each(|h| h.update(&buf[..read]));

            offset += read;
        }

        let mut itr = hashers
            .into_iter()
            .map(DynDigest::finalize)
            .map(hex::encode)
            .collect::<Vec<_>>()
            .into_iter();
        (
            itr.next().unwrap(),
            itr.next().unwrap(),
            itr.next().unwrap()
        )
    }

    #[track_caller]
    fn assert_hash_all(
        image_path: &str,
        expected_md5: &str,
        expected_sha1: &str,
        expected_sha256: &str
    ) {
        let reader = E01Reader::open_glob(image_path, false).unwrap();

        let stored_md5 = reader.get_stored_md5().map(hex::encode);
        let stored_sha1 = reader.get_stored_sha1().map(hex::encode);

        let (md5, sha1, sha256) = do_hash(&reader, false);

        assert_eq!(Some(&md5), stored_md5.as_ref());
        assert_eq!(Some(&sha1), stored_sha1.as_ref());

        assert_eq!(md5, expected_md5);
        assert_eq!(sha1, expected_sha1);
        assert_eq!(sha256, expected_sha256);
    }

    #[test]
    fn test_image_e01() {
        assert_hash_all(
            "data/image.E01",
            "28035e42858e28326c23732e6234bcf8",
            "e5c6c296485b1146fead7ad552e1c3ccfc00bfab",
            "cab8049f5fba42e06609c9d0678eb9fff7fcb50afc6c9b531ee6216bbe40a827"
        );
    }

    #[test]
    fn test_mimage_e01() {
        assert_hash_all(
            "data/mimage.E01",
            "5be32cdd1b96eac4d4a41d13234ee599",
            "f8677bd8a38a12476ae655a9f9f5336c287603f7",
            "bc730943b2247e11b18caf272b1e78289267864962751549b1722752bf1e2e3d"
        );
    }

/*
    #[test]
    fn test_dademurphy_e01() {
        assert_hash_all(
            "/home/juckelman/Downloads/dademurphy.E01",
            "caadd3db26d633249fcf9143d67d69bd",
            "109a68fc6921ea3f30aa5718177a435222b4fd15",
            "6a3720e277f54e9038b8faa5266aaa30cc5912511fbbac7256f570fa46e7060c"
        );
    }

    #[test]
    fn test_nfury_e01() {
        assert_hash_all(
            "/home/juckelman/Downloads/nfury/win7-64-nfury-c-drive.E01",
            "a98416e60bb81f57cb99125ec41bfe4c",
            "829553fd43bbd6d69c85d8285b83410ac679b066",
            "03e762e3f2732f30dd83675469129cb0a7a8e225dcbecdad1829ab4600277763"
        );
    }
*/
}
