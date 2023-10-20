pub mod e01_reader;
mod generated;

#[allow(unused)]
mod test {
    use crate::e01_reader::E01Reader;
    use sha2::Digest;
    use sha2::Sha256;
    use std::process::Command;

    extern crate rand;
    use rand::Rng;

    fn do_hash(e01_path: &str, random_buf_size: bool) -> String /*hash*/ {
        let e01_reader = E01Reader::open(&e01_path, false).unwrap();

        let mut hasher = Sha256::new();
        let mut buf: Vec<u8> = vec![0; 1048576];
        let mut offset = 0;

        while offset < e01_reader.total_size() {
            let buf_size = if random_buf_size {
                rand::thread_rng().gen_range(0..buf.len())
            } else {
                buf.len()
            };

            let readed = match e01_reader.read_at_offset(offset, &mut buf[..buf_size]) {
                Ok(v) => v,
                Err(e) => {
                    panic!("{:?}", e);
                }
            };

            if readed == 0 {
                break;
            }

            hasher.update(&buf[..readed]);

            offset += readed;
        }
        let result = hasher.finalize();
        format!("{:X}", result)
    }

    #[test]
    fn test_all_images() {
        do_hash_both("data/image.E01");
        do_hash_both("data/mimage.E01");
    }

    fn do_hash_both(image_path: &str) {
        let hash_libewf = do_hash_libewf(image_path);
        assert_eq!(do_hash(image_path, false), hash_libewf);
        assert_eq!(do_hash(image_path, true), hash_libewf);
    }

    fn do_hash_libewf(image_path: &str) -> String {
        if cfg!(target_os = "windows") {
            let hash = Command::new("tools/ewfverify.exe")
                .arg("-d")
                .arg("sha256")
                .arg("-q")
                .arg(image_path.replace("/", "\\"))
                .output()
                .expect("Failed to execute ewfverify.exe");
            if !hash.status.success() {
                panic!(
                    "ewfverify.exe failed: {}",
                    String::from_utf8(hash.stderr).unwrap()
                );
            }
            String::from_utf8(hash.stdout)
                .unwrap()
                .lines()
                .skip(4)
                .next()
                .unwrap()
                .split("\t")
                .last()
                .unwrap()
                .trim()
                .to_string()
                .replace("\"", "")
                .to_uppercase()
        } else {
            "".to_string()
        }
    }
}
