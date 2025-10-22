use async_trait::async_trait;

#[async_trait]
pub trait Cache {
    async fn read(
        &mut self,
        idx: usize,
        off: u64,
        buf: &mut [u8]
    ) -> Result<(), std::io::Error>;

    fn end(&self, idx: usize) -> Result<u64, std::io::Error>;
}
