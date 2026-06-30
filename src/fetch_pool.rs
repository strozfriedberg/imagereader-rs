use std::collections::HashMap;
use std::future::Future;
use std::sync::Arc;

use futures::future::{BoxFuture, FutureExt, Shared};
use tokio::sync::{Mutex, Semaphore};

/// Foyer block identity: `(source/extent index, segment-file byte offset)`.
pub type FetchKey = (usize, u64);

type InflightFetch = Shared<BoxFuture<'static, Result<Vec<u8>, String>>>;

/// Limits concurrent backing-store reads and deduplicates in-flight fetches per key.
pub struct FetchPool {
    semaphore: Arc<Semaphore>,
    inflight: Mutex<HashMap<FetchKey, InflightFetch>>,
}

impl FetchPool {
    /// `max_inflight == 0` means serial (one permit).
    pub fn new(max_inflight: usize) -> Arc<Self> {
        let permits = if max_inflight == 0 { 1 } else { max_inflight };
        Arc::new(Self {
            semaphore: Arc::new(Semaphore::new(permits)),
            inflight: Mutex::new(HashMap::new()),
        })
    }

    pub async fn run<F, Fut>(
        self: &Arc<Self>,
        key: FetchKey,
        fetch: F,
    ) -> Result<Vec<u8>, std::io::Error>
    where
        F: FnOnce() -> Fut + Send + 'static,
        Fut: Future<Output = Result<Vec<u8>, std::io::Error>> + Send + 'static,
    {
        loop {
            {
                let guard = self.inflight.lock().await;
                if let Some(shared) = guard.get(&key) {
                    return shared.clone().await.map_err(std::io::Error::other);
                }
            }

            let sem = Arc::clone(&self.semaphore);
            let shared = async move {
                let _permit = sem
                    .acquire()
                    .await
                    .map_err(|_| "fetch pool semaphore closed".to_string())?;
                fetch().await.map_err(|e| e.to_string())
            }
            .boxed()
            .shared();

            {
                let mut guard = self.inflight.lock().await;
                if let Some(existing) = guard.get(&key) {
                    return existing.clone().await.map_err(std::io::Error::other);
                }
                guard.insert(key, shared.clone());
            }

            let result = shared.await.map_err(std::io::Error::other);
            self.inflight.lock().await.remove(&key);
            return result;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[tokio::test]
    async fn dedupes_concurrent_fetches_for_same_key() {
        let pool = FetchPool::new(4);
        let calls = Arc::new(AtomicUsize::new(0));
        let key = (0, 0);

        let c1 = calls.clone();
        let p1 = pool.clone();
        let h1 = tokio::spawn(async move {
            p1.run(key, move || async move {
                c1.fetch_add(1, Ordering::SeqCst);
                tokio::time::sleep(std::time::Duration::from_millis(50)).await;
                Ok(vec![1, 2, 3])
            })
            .await
        });

        let c2 = calls.clone();
        let p2 = pool.clone();
        let h2 = tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
            p2.run(key, move || async move {
                c2.fetch_add(1, Ordering::SeqCst);
                Ok(vec![9])
            })
            .await
        });

        let r1 = h1.await.unwrap().unwrap();
        let r2 = h2.await.unwrap().unwrap();
        assert_eq!(r1, r2);
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn serial_when_max_inflight_zero() {
        let pool = FetchPool::new(0);
        let active = Arc::new(AtomicUsize::new(0));
        let max_active = Arc::new(AtomicUsize::new(0));

        let mut handles = vec![];
        for i in 0u64..4 {
            let pool = pool.clone();
            let active = active.clone();
            let max_active = max_active.clone();
            handles.push(tokio::spawn(async move {
                pool.run((0, i), move || async move {
                    let now = active.fetch_add(1, Ordering::SeqCst) + 1;
                    max_active.fetch_max(now, Ordering::SeqCst);
                    tokio::time::sleep(std::time::Duration::from_millis(20)).await;
                    active.fetch_sub(1, Ordering::SeqCst);
                    Ok(vec![i as u8])
                })
                .await
            }));
        }

        for h in handles {
            h.await.unwrap().unwrap();
        }
        assert_eq!(max_active.load(Ordering::SeqCst), 1);
    }
}
