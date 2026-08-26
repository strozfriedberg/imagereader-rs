pub mod e01_reader;

#[cfg(feature = "capi")]
pub mod capi;

#[cfg(test)]
mod test_data;

#[cfg(test)]
mod test_helper;

mod cacheworkersource;
mod error;
mod generated;
pub mod hasher;
mod readworker;
mod sec_read;
mod seg_path;
mod segment;
mod workersource;

pub use imagesource::{
    IoLog, ReadTimer, ReadTrace, aligned_fetch_size, chunk_cache_label, init_tracing,
};

#[cfg(test)]
mod test {
    use crate::{
        e01_reader::{
            CacheMode, CorruptChunkPolicy, CorruptSectionPolicy, DEFAULT_CACHE_BLOCK_SIZE,
            DEFAULT_CACHE_FETCH_SIZE, DEFAULT_CACHE_MEM_MIB, DEFAULT_PARALLEL_CHUNK_THREADS,
            DEFAULT_S3_CONCURRENCY, E01Reader, E01ReaderOptions,
        },
        hasher::HashType,
        test_data::*,
        test_helper::do_hash,
    };

    #[track_caller]
    fn assert_eq_test_data(exp: &TestData, options: &E01ReaderOptions) {
        let reader = E01Reader::open_glob(exp.segment_paths[0], options).unwrap();
        assert_eq_reader(&reader, exp);
    }

    #[track_caller]
    fn assert_eq_test_data_nonglob(exp: &TestData, options: &E01ReaderOptions) {
        let reader = E01Reader::open(exp.segment_paths, options).unwrap();
        assert_eq_reader(&reader, exp);
    }

    #[track_caller]
    fn assert_eq_reader(reader: &E01Reader, exp: &TestData) {
        let image_size = reader.image_size;

        let hashes = do_hash(
            |offset, buf: &mut [u8]| reader.read_at_offset(offset, buf).unwrap(),
            image_size,
            false,
        );

        let stored_md5 = reader.stored_md5.map(hex::encode);
        let stored_sha1 = reader.stored_sha1.map(hex::encode);

        let segment_paths = reader
            .segment_paths
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
            sha256: hashes.get(&HashType::SHA256).map(String::as_str),
        };

        assert_eq!(&act, exp);
    }

    fn error_error() -> E01ReaderOptions {
        E01ReaderOptions {
            corrupt_section_policy: CorruptSectionPolicy::Error,
            corrupt_chunk_policy: CorruptChunkPolicy::Error,
            foyer_readahead: 0,
            s3_concurrency: DEFAULT_S3_CONCURRENCY,
            cache_mem_mib: DEFAULT_CACHE_MEM_MIB,
            cache_block_size: DEFAULT_CACHE_BLOCK_SIZE,
            cache_fetch_size: DEFAULT_CACHE_FETCH_SIZE,
            cache_mode: CacheMode::default(),
            cache_dir: None,
            io_log: None,
            parallel_chunk_reads: true,
            parallel_chunk_threads: DEFAULT_PARALLEL_CHUNK_THREADS,
            decoded_chunk_cache: true,
        }
    }

    fn error_zero() -> E01ReaderOptions {
        E01ReaderOptions {
            corrupt_section_policy: CorruptSectionPolicy::Error,
            corrupt_chunk_policy: CorruptChunkPolicy::Zero,
            foyer_readahead: 0,
            s3_concurrency: DEFAULT_S3_CONCURRENCY,
            cache_mem_mib: DEFAULT_CACHE_MEM_MIB,
            cache_block_size: DEFAULT_CACHE_BLOCK_SIZE,
            cache_fetch_size: DEFAULT_CACHE_FETCH_SIZE,
            cache_mode: CacheMode::default(),
            cache_dir: None,
            io_log: None,
            parallel_chunk_reads: true,
            parallel_chunk_threads: DEFAULT_PARALLEL_CHUNK_THREADS,
            decoded_chunk_cache: true,
        }
    }

    #[test]
    fn test_image_e01() {
        assert_eq_test_data(&IMAGE_E01, &error_error());
    }

    #[test]
    fn test_mimage_e01() {
        assert_eq_test_data(&MIMAGE_E01, &error_error());
    }

    #[test]
    fn test_mimage_e01_nonglob() {
        assert_eq_test_data_nonglob(&MIMAGE_E01, &error_error());
    }

    #[test]
    fn test_mimage_e01_zero_bad_chunks() {
        assert_eq_test_data(&MIMAGE_E01, &error_zero());
    }

    #[test]
    #[should_panic]
    fn test_bad_chunk_e01() {
        assert_eq_test_data(&BAD_CHUNK_E01, &error_error());
    }

    #[test]
    fn test_bad_chunk_e01_zero_bad_chunks() {
        assert_eq_test_data(&BAD_CHUNK_E01_ZEROED, &error_zero());
    }
}
