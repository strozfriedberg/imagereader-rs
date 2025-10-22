use async_trait::async_trait;
use foyer::{DefaultHasher, HybridCache};
use foyer_common::code::HashBuilder;
use futures::future::{TryFutureExt, try_join_all};
use std::{
    fmt::Debug,
    rc::Rc
};
use tracing::trace;

use crate::bytessource::BytesSource;
use crate::cache::Cache;

#[derive(Debug)]
pub struct FoyerCache<F, S = DefaultHasher>
where
    F: BytesSource + Clone + Send,
    S: HashBuilder + Debug
{
    chlen: usize,
    sources: Vec<F>,
    cache: Rc<HybridCache<(usize, u64), Vec<u8>, S>>,
}

impl<F, S> FoyerCache<F, S>
where
    F: BytesSource + Clone + Send,
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
            cache: Rc::new(cache)
        }
    }

    pub fn add_source(&mut self, src: F) {
        self.sources.push(src);
    }
}

#[async_trait]
impl<F, S> Cache for FoyerCache<F, S>
where
    F: BytesSource + Clone + Send,
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
            .ok_or(std::io::Error::other(format!("{idx} out of bounds")))?
            .clone();

        let end = source.end();

        let cache = self.cache.clone();
        
        let chlen = self.chlen;

        let getter = move |choff: u64| {
            let cache = cache.clone();
            let source = source.clone();

            let fetch = move ||
                source.read(choff, (choff + chlen as u64).min(end))
                    .map_err(foyer::Error::other::<std::io::Error>);

            trace!("fetching {idx} [{choff},{})", choff + chlen as u64);
            cache.fetch((idx, choff), fetch)
        }; 

        let csbeg = (off / self.chlen as u64) * self.chlen as u64;
        let csend = off + buf.len() as u64;

        let fut = try_join_all((csbeg..csend).step_by(chlen).map(getter));

        let chunks = fut.await.map_err(std::io::Error::other)?;

/*
        let futs = (csbeg..csend).step_by(chlen).map(getter).collect::<Vec<_>>();
        trace!("{}", line!());
        let mut r = vec![];
        for f in futs {
            r.push(f.await);    
        }

        trace!("{}", line!());
        let mut chunks = vec![];
        for x in r {
            chunks.push(x.map_err(std::io::Error::other)?);
        }

        trace!("{}", line!());
*/
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
}
