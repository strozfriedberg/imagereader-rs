use std::sync::Arc;
use std::time::Instant;

use futures::future::{BoxFuture, FutureExt};
use s3::{bucket::Bucket, request::request_trait::ResponseData};
use tokio::sync::RwLock;
use tracing::trace;

use crate::bytessource::BytesSource;
use crate::io_log::IoLog;
use crate::s3_creds::{S3Auth, ensure_fresh_async};

pub struct S3Source {
    bucket: Arc<RwLock<Bucket>>,
    path: String,
    len: u64,
    auth: Arc<S3Auth>,
    segment: usize,
    io_log: Option<Arc<IoLog>>,
}

impl S3Source {
    pub fn new(
        bucket: Bucket,
        path: String,
        len: u64,
        auth: Arc<S3Auth>,
        segment: usize,
        io_log: Option<Arc<IoLog>>,
    ) -> Self {
        Self {
            bucket: Arc::new(RwLock::new(bucket)),
            path,
            len,
            auth,
            segment,
            io_log,
        }
    }

    async fn sync_credentials(
        bucket: &Arc<RwLock<Bucket>>,
        auth: &S3Auth,
    ) -> Result<(), std::io::Error> {
        ensure_fresh_async(auth).await?;
        let creds = auth.shared_creds.read().await.clone();
        let needs_update = {
            let bucket = bucket.read().await;
            match bucket.credentials().await {
                Ok(current) => {
                    current.access_key != creds.access_key
                        || current.secret_key != creds.secret_key
                        || current.session_token != creds.session_token
                }
                Err(_) => true,
            }
        };
        if needs_update {
            bucket.write().await.set_credentials(creds);
        }
        Ok(())
    }
}

/// Validate an S3 range response. A well-formed range read comes back as
/// 206 Partial Content with exactly `end - beg` bytes. An endpoint or proxy
/// that ignores the Range header answers 200 with the whole object; serving
/// (and caching) those misaligned bytes is worse than failing, so reject
/// anything that is not a correctly-sized 206.
fn check_range_response(beg: u64, end: u64, status: u16, got: usize) -> Result<(), std::io::Error> {
    let wanted = (end - beg) as usize;
    if status != 206 || got != wanted {
        return Err(std::io::Error::other(format!(
            "S3 range [{beg}, {end}) returned status {status} with {got} bytes, expected 206 with {wanted}"
        )));
    }
    Ok(())
}

impl BytesSource for S3Source {
    fn read(&self, beg: u64, end: u64) -> BoxFuture<'static, Result<Vec<u8>, std::io::Error>> {
        let bucket = self.bucket.clone();
        let path = self.path.clone();
        let auth = self.auth.clone();
        let segment = self.segment;
        let io_log = self.io_log.clone();
        async move {
            let start = Instant::now();
            Self::sync_credentials(&bucket, &auth).await?;

            let result = if end <= beg {
                // Empty or inverted range. get_object_range would underflow on
                // `end - 1` and trip rust-s3's assert!(start <= end); a corrupt
                // extent table or a zero-byte object must surface as an error,
                // not a panic inside the fetch.
                Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    format!("invalid S3 range [{beg}, {end})"),
                ))
            } else {
                let bucket = bucket.read().await;
                match bucket
                    .get_object_range(
                        &path,
                        beg,
                        Some(end - 1), // inclusive, augh!
                    )
                    .await
                {
                    Ok(resp) => {
                        let status = resp.status_code();
                        let data = ResponseData::to_vec(resp);
                        check_range_response(beg, end, status, data.len()).map(|()| {
                            trace!("read [{beg},{end}) from S3");
                            data
                        })
                    }
                    Err(e) => Err(std::io::Error::other(e)),
                }
            };

            if let Some(log) = io_log {
                let dur_us = start.elapsed().as_micros().min(u64::MAX as u128) as u64;
                log.log_s3_fetch(segment, beg, end, dur_us);
            }
            result
        }
        .boxed()
    }

    fn end(&self) -> u64 {
        self.len
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn correct_206_response_is_accepted() {
        assert!(check_range_response(0, 4096, 206, 4096).is_ok());
    }

    #[test]
    fn status_200_whole_object_is_rejected() {
        // endpoint ignored Range and returned the whole (larger) object
        let err = check_range_response(0, 4096, 200, 1 << 20).unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::Other);
    }

    #[test]
    fn short_206_body_is_rejected() {
        // 206 but fewer bytes than requested (truncated object)
        assert!(check_range_response(0, 4096, 206, 2048).is_err());
    }
}
