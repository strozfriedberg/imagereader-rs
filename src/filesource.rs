use std::{
    fs::File,
    io::{Read, Seek, SeekFrom}
};

use crate::bytessource::BytesSource;

pub struct FileSource {
    handle: File
}

impl FileSource {
    pub fn new(f: File) -> Self {
        Self { handle: f }
    }
}

impl BytesSource for FileSource {
    fn read(
        &mut self,
        off: u64,
        buf: &mut [u8]
    ) -> Result<(), std::io::Error>
    {
        self.handle.seek(SeekFrom::Start(off))?;
        self.handle.read_exact(buf)
    }
}
