pub trait Cache {
/*
    fn read_blocking(
        &mut self,
        idx: usize,
        off: u64,
        buf: &mut [u8]
    ) -> Result<(), std::io::Error>
    {
        let rt = tokio::runtime::Runtime::new()
            .map_err(std::io::Error::other)?;
        rt.block_on(self.read(idx, off, buf))
    }
*/
    async fn read(
        &mut self,
        idx: usize,
        off: u64,
        buf: &mut [u8]
    ) -> Result<(), std::io::Error>;

    fn end(&self, idx: usize) -> Result<u64, std::io::Error>;
}
