use std::future::Future;
use std::sync::Arc;

use tokio::sync::Semaphore;

/// Bounds how many backing-store reads (S3 range GETs, file reads) are in
/// flight at once.
///
/// This is here as a resource bound (memory, file descriptors, etc); foyer
/// spawns a task per miss and will not stop at any particular number.
///
/// Deduplicating concurrent fetches of the same block is deliberately *not*
/// done here: foyer's `get_or_fetch` already coalesces them, in the same lock
/// critical section as the cache lookup, and it drives the winning fetch on its
/// own spawned task so a dropped caller can't cancel it or strand the entry.
pub struct FetchLimiter {
    semaphore: Semaphore,
}

impl FetchLimiter {
    /// `max_inflight == 0` means serial (one permit).
    pub fn new(max_inflight: usize) -> Arc<Self> {
        let permits = if max_inflight == 0 { 1 } else { max_inflight };
        Arc::new(Self {
            semaphore: Semaphore::new(permits),
        })
    }

    pub async fn run<F, Fut>(&self, fetch: F) -> Result<Vec<u8>, std::io::Error>
    where
        F: FnOnce() -> Fut + Send + 'static,
        Fut: Future<Output = Result<Vec<u8>, std::io::Error>> + Send + 'static,
    {
        let _permit = self
            .semaphore
            .acquire()
            .await
            .map_err(|_| std::io::Error::other("fetch limiter semaphore closed"))?;

        fetch().await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[tokio::test]
    async fn caps_concurrent_fetches() {
        let limiter = FetchLimiter::new(2);
        let active = Arc::new(AtomicUsize::new(0));
        let max_active = Arc::new(AtomicUsize::new(0));

        let mut handles = vec![];
        for i in 0u64..8 {
            let limiter = limiter.clone();
            let active = active.clone();
            let max_active = max_active.clone();
            handles.push(tokio::spawn(async move {
                limiter
                    .run(move || async move {
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

        assert_eq!(max_active.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn serial_when_max_inflight_zero() {
        let limiter = FetchLimiter::new(0);
        let active = Arc::new(AtomicUsize::new(0));
        let max_active = Arc::new(AtomicUsize::new(0));

        let mut handles = vec![];
        for i in 0u64..4 {
            let limiter = limiter.clone();
            let active = active.clone();
            let max_active = max_active.clone();
            handles.push(tokio::spawn(async move {
                limiter
                    .run(move || async move {
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

    /// The old FetchPool flattened io::Error into String, losing the kind.
    #[tokio::test]
    async fn preserves_the_io_error_kind() {
        let limiter = FetchLimiter::new(4);

        let err = limiter
            .run(|| async {
                Err(std::io::Error::new(
                    std::io::ErrorKind::UnexpectedEof,
                    "truncated",
                ))
            })
            .await
            .unwrap_err();

        assert_eq!(err.kind(), std::io::ErrorKind::UnexpectedEof);
    }
}
