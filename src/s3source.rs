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
            let bucket = bucket.read().await;
            let result = bucket
                .get_object_range(
                    &path,
                    beg,
                    Some(end - 1), // inclusive, augh!
                )
                .await
                .inspect(|_| trace!("read [{beg},{end}) from S3"))
                .map(ResponseData::to_vec)
                .map_err(std::io::Error::other);
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
