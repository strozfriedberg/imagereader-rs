//! Shared infrastructure for disk-image readers: byte sources (local file,
//! direct-from-S3), a foyer-based hybrid cache with a protected metadata tier,
//! AWS credential resolution, and I/O logging.

pub mod bytessource;
pub mod cache;
pub mod cachemode;
pub mod cachereadseek;
pub mod dummycache;
pub mod errors;
pub mod fetch_limit;
pub mod filesource;
pub mod foyercache;
pub mod io_log;
pub mod placeholdersource;
pub mod readseek;
pub mod s3_creds;
pub mod s3source;
pub mod tracing_init;
pub mod urlsource;

pub use bytessource::BytesSource;
pub use cache::Cache;
pub use cachemode::{
    CacheMode, DEFAULT_CACHE_CHUNK_SIZE, DEFAULT_CACHE_MEM_MIB, DEFAULT_S3_CONCURRENCY,
};
pub use cachereadseek::CacheReadSeek;
pub use dummycache::DummyCache;
pub use errors::{InitError, OpenError, OpenErrorKind};
pub use fetch_limit::FetchLimiter;
pub use filesource::FileSource;
pub use foyercache::FoyerCache;
pub use io_log::{IoLog, ReadTimer, ReadTrace, chunk_cache_label};
pub use placeholdersource::PlaceholderSource;
pub use readseek::ReadSeek;
pub use s3_creds::{S3Auth, resolve_s3_auth, s3_region_name, snapshot_credentials_sync};
pub use s3source::S3Source;
pub use tracing_init::init as init_tracing;
pub use urlsource::{path_or_url_to_url, s3_bucket, source_for_url};
