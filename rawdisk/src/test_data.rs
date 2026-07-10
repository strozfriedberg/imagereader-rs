#![cfg(test)]

#[derive(Debug, PartialEq, Eq)]
pub struct TestData<'a> {
    pub image_path: &'a str,
    pub image_size: u64,
    pub sha1: &'a str,
}

/// 4 MiB, byte i = (i*7+3) % 251. Chunk-aligned end.
pub const PATTERNED_4MIB: TestData = TestData {
    image_path: "data/patterned_4mib.raw",
    image_size: 4194304,
    sha1: "c55b0fd65b06ab87049a817943e220da673b1198",
};

/// 3 MiB + 512 B, same pattern. End is not chunk-aligned, exercising the
/// end-of-image clamp in both the reader and the cache block math.
pub const UNALIGNED_3MIB_512B: TestData = TestData {
    image_path: "data/unaligned_3mib_512b.raw",
    image_size: 3146240,
    sha1: "3dae0a374f400808d2e0f003e7e0ba90b2140b22",
};
