use async_trait::async_trait;
use foyer::{
    BlockEngineBuilder,
    DefaultHasher,
    DeviceBuilder,
    FsDeviceBuilder,
    HybridCache,
    HybridCacheBuilder
};
use foyer_common::code::HashBuilder;
use futures::future::{TryFutureExt, try_join_all};
use std::{
    fmt::Debug,
    sync::Arc
};
use tracing::trace;

use crate::{
    bytessource::BytesSource,
    cache::Cache,
    placeholdersource::PlaceholderSource
};

pub struct FoyerCache<S = DefaultHasher>
where
    S: HashBuilder + Debug
{
    chlen: usize,
    sources: Vec<Box<dyn BytesSource + Send>>,
    cache: Arc<HybridCache<(usize, u64), Vec<u8>, S>>,
}

impl FoyerCache<DefaultHasher> {
    pub async fn with_default_cache(chlen: usize) -> Self {
        let dir = "cache";

        let device = FsDeviceBuilder::new(dir)
            .with_capacity(256 * 1024 * 1024)
            .build()
            .unwrap();

        let cache = HybridCacheBuilder::new()
            .memory(64 * 1024 * 1024)
            .storage()
            .with_engine_config(BlockEngineBuilder::new(device))
            .build()
            .await
        .unwrap();

        Self::new(cache, chlen)
    }
}

impl<S> FoyerCache<S>
where
    S: HashBuilder + Debug
{
    pub fn new(
        cache: HybridCache<(usize, u64), Vec<u8>, S>,
        chlen: usize
    ) -> Self
    {
        Self {
            chlen,
            sources: vec![],
            cache: Arc::new(cache)
        }
    }
}

#[async_trait]
impl<S> Cache for FoyerCache<S>
where
    S: HashBuilder + Debug
{
    async fn read(
        &mut self,
        idx: usize,
        off: u64,
        buf: &mut [u8]
    ) -> Result<(), std::io::Error>
    {
        let source = self.sources.get(idx)
            .ok_or(std::io::Error::other(format!("{idx} out of bounds")))?;

        let end = source.end();

        let cache = self.cache.clone();

        let chlen = self.chlen;

        let getter = move |choff: u64| {
            let fetch = move ||
                source.read(choff, (choff + chlen as u64).min(end))
                    .map_err(foyer::Error::other::<std::io::Error>);

            trace!("fetching {idx} [{choff},{})", (choff + chlen as u64).min(end));
            cache.fetch((idx, choff), fetch)
        };

        let csbeg = (off / self.chlen as u64) * self.chlen as u64;
        let csend = off + buf.len() as u64;

        let fut = try_join_all((csbeg..csend).step_by(chlen).map(getter));

        let chunks = fut.await.map_err(std::io::Error::other)?;

        let mut bbeg = 0;

        for (choff, ch) in (csbeg..csend).step_by(self.chlen).zip(chunks) {
            trace!("fetched {idx} [{choff},{})", choff + ch.len() as u64);

            let chbeg = (off + bbeg) - choff;
            let chend = (chbeg + (buf.len() as u64 - bbeg)).min(ch.len() as u64);
            let bend = bbeg + (chend - chbeg);

            buf[bbeg as usize..bend as usize].copy_from_slice(&ch[chbeg as usize..chend as usize]);

            trace!("filled [{},{})", off + bbeg, off + bend);
            bbeg += chend - chbeg;
        }

        Ok(())
    }

    fn end(&self, idx: usize) -> Result<u64, std::io::Error> {
        self.sources.get(idx)
            .ok_or(std::io::Error::other(format!("{idx} out of bounds")))
            .map(|src| src.end())
    }

    fn add_source(&mut self, idx: usize, src: Box<dyn BytesSource + Send>) {
        if self.sources.len() <= idx {
            self.sources.resize_with(idx + 1, || Box::new(PlaceholderSource));
        }
        self.sources[idx] = src;
    }
}
