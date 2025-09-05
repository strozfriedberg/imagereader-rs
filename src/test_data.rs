
#[derive(Debug, PartialEq, Eq)]
pub struct TestData<'a> {
    pub segment_paths: &'a [&'a str],
    pub chunk_size: usize,
    pub chunk_count: usize,
    pub sector_size: usize,
    pub sector_count: usize,
    pub image_size: usize,
    pub stored_md5: Option<&'a str>,
    pub stored_sha1: Option<&'a str>,
    pub md5: Option<&'a str>,
    pub sha1: Option<&'a str>,
    pub sha256: Option<&'a str>
}

pub const IMAGE_E01: TestData = TestData {
    segment_paths: &["data/image.E01"],
    chunk_size: 32768,
    chunk_count: 41,
    sector_size: 512,
    sector_count: 2581,
    image_size: 1321472,
    stored_md5: Some("28035e42858e28326c23732e6234bcf8"),
    stored_sha1: Some("e5c6c296485b1146fead7ad552e1c3ccfc00bfab"),
    md5: Some("28035e42858e28326c23732e6234bcf8"),
    sha1: Some("e5c6c296485b1146fead7ad552e1c3ccfc00bfab"),
    sha256: Some("cab8049f5fba42e06609c9d0678eb9fff7fcb50afc6c9b531ee6216bbe40a827")
};

pub const MIMAGE_E01: TestData = TestData {
    segment_paths: &["data/mimage.E01", "data/mimage.E02"],
    chunk_size: 32768,
    chunk_count: 27,
    sector_size: 512,
    sector_count: 1728,
    image_size: 884736,
    stored_md5: Some("5be32cdd1b96eac4d4a41d13234ee599"),
    stored_sha1: Some("f8677bd8a38a12476ae655a9f9f5336c287603f7"),
    md5: Some("5be32cdd1b96eac4d4a41d13234ee599"),
    sha1: Some("f8677bd8a38a12476ae655a9f9f5336c287603f7"),
    sha256: Some("bc730943b2247e11b18caf272b1e78289267864962751549b1722752bf1e2e3d")
};

pub const BAD_CHUNK_E01: TestData = TestData {
    segment_paths: &["data/bad_chunk.E01"],
    chunk_size: 32768,
    chunk_count: 41,
    sector_size: 512,
    sector_count: 2581,
    image_size: 1321472,
    stored_md5: Some("28035e42858e28326c23732e6234bcf8"),
    stored_sha1: Some("e5c6c296485b1146fead7ad552e1c3ccfc00bfab"),
    md5: None,
    sha1: None,
    sha256: None
};

pub const BAD_CHUNK_E01_ZEROED: TestData = TestData {
    segment_paths: &["data/bad_chunk.E01"],
    chunk_size: 32768,
    chunk_count: 41,
    sector_size: 512,
    sector_count: 2581,
    image_size: 1321472,
    stored_md5: Some("28035e42858e28326c23732e6234bcf8"),
    stored_sha1: Some("e5c6c296485b1146fead7ad552e1c3ccfc00bfab"),
    md5: Some("67c44c58dd4bb4f7d162b3d3ad521e33"),
    sha1: Some("18e70fcac21668a2ee849cdb815d45dab107f0fc"),
    sha256: Some("077861781adaad81e64b229111ef4a490884eecee74eb7c91fed5d291995caf2")
};
