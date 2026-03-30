pub trait WorkerSource {
    fn read(
        &mut self,
        off: u64,
        buf: &mut [u8]
    ) -> Result<(), std::io::Error>;
}
