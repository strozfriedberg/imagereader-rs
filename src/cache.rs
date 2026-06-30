use async_trait::async_trait;

use crate::bytessource::BytesSource;
use crate::io_log::ReadTrace;

#[async_trait]
pub trait Cache {
    async fn read(
        &mut self,
        idx: usize,
        off: u64,
        buf: &mut [u8],
        trace: &mut ReadTrace,
    ) -> Result<(), std::io::Error>;

    fn end(&self, idx: usize) -> Result<u64, std::io::Error>;

    fn add_source(&mut self, idx: usize, src: Box<dyn BytesSource + Send + Sync>);
}
