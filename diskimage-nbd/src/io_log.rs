use std::{
    fs::File,
    io::{BufWriter, Write},
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::Instant,
};

use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

/// Per-read cache outcome collected while serving one `read_at_offset`.
#[derive(Debug, Default, Clone, Copy)]
pub struct ReadTrace {
    pub foyer_miss: bool,
}

impl ReadTrace {
    pub fn foyer_label(self) -> &'static str {
        if self.foyer_miss { "miss" } else { "hit" }
    }
}

pub fn chunk_cache_label(hit: Option<bool>) -> &'static str {
    match hit {
        None => "n/a",
        Some(true) => "hit",
        Some(false) => "miss",
    }
}

/// Append-only JSONL trace of NBD traffic and backing-store fetches.
///
/// S3 fetches during open are written then discarded on the first NBD
/// client connect (`begin_serving`), so the log reflects post-connect workload.
#[derive(Debug)]
pub struct IoLog {
    path: PathBuf,
    writer: Mutex<BufWriter<File>>,
    serving: AtomicBool,
    nbd_reads: AtomicU64,
    nbd_read_bytes: AtomicU64,
    reads: AtomicU64,
    read_bytes: AtomicU64,
    s3_fetches: AtomicU64,
    s3_bytes: AtomicU64,
    foyer_hits: AtomicU64,
    foyer_misses: AtomicU64,
    chunk_hits: AtomicU64,
    chunk_misses: AtomicU64,
    prefetch_enqueued: AtomicU64,
}

impl IoLog {
    pub fn open(path: &Path) -> std::io::Result<Arc<Self>> {
        let file = File::options().create(true).append(true).open(path)?;
        Ok(Arc::new(Self {
            path: path.to_path_buf(),
            writer: Mutex::new(BufWriter::new(file)),
            serving: AtomicBool::new(false),
            nbd_reads: AtomicU64::new(0),
            nbd_read_bytes: AtomicU64::new(0),
            reads: AtomicU64::new(0),
            read_bytes: AtomicU64::new(0),
            s3_fetches: AtomicU64::new(0),
            s3_bytes: AtomicU64::new(0),
            foyer_hits: AtomicU64::new(0),
            foyer_misses: AtomicU64::new(0),
            chunk_hits: AtomicU64::new(0),
            chunk_misses: AtomicU64::new(0),
            prefetch_enqueued: AtomicU64::new(0),
        }))
    }

    pub fn record_prefetch_enqueued(&self, count: u64) {
        if count > 0 {
            self.prefetch_enqueued.fetch_add(count, Ordering::Relaxed);
        }
    }

    pub fn log_prefetch(&self, segment: usize, block_off: u64, offsets: &[u64]) {
        if !self.serving.load(Ordering::Relaxed) || offsets.is_empty() {
            return;
        }
        let count = offsets.len() as u64;
        self.prefetch_enqueued.fetch_add(count, Ordering::Relaxed);
        let offs = offsets
            .iter()
            .map(|o| o.to_string())
            .collect::<Vec<_>>()
            .join(",");
        let _ = self.write_line(&format!(
            r#"{{"kind":"prefetch","segment":{segment},"block":{block_off},"offsets":[{offs}],"count":{count}}}"#
        ));
    }

    /// Drop open-phase events; subsequent lines are the NBD serving workload.
    pub fn begin_serving(&self) -> std::io::Result<()> {
        if self.serving.swap(true, Ordering::SeqCst) {
            return Ok(());
        }
        self.nbd_reads.store(0, Ordering::Relaxed);
        self.nbd_read_bytes.store(0, Ordering::Relaxed);
        self.reads.store(0, Ordering::Relaxed);
        self.read_bytes.store(0, Ordering::Relaxed);
        self.s3_fetches.store(0, Ordering::Relaxed);
        self.s3_bytes.store(0, Ordering::Relaxed);
        self.foyer_hits.store(0, Ordering::Relaxed);
        self.foyer_misses.store(0, Ordering::Relaxed);
        self.chunk_hits.store(0, Ordering::Relaxed);
        self.chunk_misses.store(0, Ordering::Relaxed);
        self.prefetch_enqueued.store(0, Ordering::Relaxed);

        let file = File::options()
            .create(true)
            .write(true)
            .truncate(true)
            .open(&self.path)?;
        {
            let mut w = self
                .writer
                .lock()
                .map_err(|_| std::io::Error::other("io log lock poisoned"))?;
            *w = BufWriter::new(file);
        }
        self.write_line(r#"{"kind":"marker","event":"nbd_connected"}"#)
    }

    pub fn log_nbd_read(&self, offset: u64, len: u32, dur_us: u64) {
        if !self.serving.load(Ordering::Relaxed) {
            return;
        }
        self.nbd_reads.fetch_add(1, Ordering::Relaxed);
        self.nbd_read_bytes.fetch_add(len as u64, Ordering::Relaxed);
        let _ = self.write_line(&format!(
            r#"{{"kind":"nbd_read","offset":{offset},"len":{len},"dur_us":{dur_us}}}"#
        ));
    }

    pub fn log_read(&self, offset: u64, len: usize, dur_us: u64, foyer: &str, chunk: &str) {
        if !self.serving.load(Ordering::Relaxed) {
            return;
        }
        self.reads.fetch_add(1, Ordering::Relaxed);
        self.read_bytes.fetch_add(len as u64, Ordering::Relaxed);
        match foyer {
            "hit" => {
                self.foyer_hits.fetch_add(1, Ordering::Relaxed);
            }
            "miss" => {
                self.foyer_misses.fetch_add(1, Ordering::Relaxed);
            }
            _ => {}
        }
        match chunk {
            "hit" => {
                self.chunk_hits.fetch_add(1, Ordering::Relaxed);
            }
            "miss" => {
                self.chunk_misses.fetch_add(1, Ordering::Relaxed);
            }
            _ => {}
        }
        let _ = self.write_line(&format!(
            r#"{{"kind":"read","offset":{offset},"len":{len},"dur_us":{dur_us},"foyer":"{foyer}","chunk":"{chunk}"}}"#
        ));
    }

    pub fn log_s3_fetch(&self, segment: usize, beg: u64, end: u64, dur_us: u64) {
        if !self.serving.load(Ordering::Relaxed) {
            return;
        }
        let bytes = end.saturating_sub(beg);
        self.s3_fetches.fetch_add(1, Ordering::Relaxed);
        self.s3_bytes.fetch_add(bytes, Ordering::Relaxed);
        let _ = self.write_line(&format!(
            r#"{{"kind":"s3","segment":{segment},"beg":{beg},"end":{end},"bytes":{bytes},"dur_us":{dur_us}}}"#
        ));
    }

    pub fn log_summary(&self) {
        if !self.serving.load(Ordering::Relaxed) {
            return;
        }
        let nbd_reads = self.nbd_reads.load(Ordering::Relaxed);
        let nbd_read_bytes = self.nbd_read_bytes.load(Ordering::Relaxed);
        let reads = self.reads.load(Ordering::Relaxed);
        let read_bytes = self.read_bytes.load(Ordering::Relaxed);
        let s3_fetches = self.s3_fetches.load(Ordering::Relaxed);
        let s3_bytes = self.s3_bytes.load(Ordering::Relaxed);
        let foyer_hits = self.foyer_hits.load(Ordering::Relaxed);
        let foyer_misses = self.foyer_misses.load(Ordering::Relaxed);
        let chunk_hits = self.chunk_hits.load(Ordering::Relaxed);
        let chunk_misses = self.chunk_misses.load(Ordering::Relaxed);
        let prefetch_enqueued = self.prefetch_enqueued.load(Ordering::Relaxed);
        let _ = self.write_line(&format!(
            r#"{{"kind":"summary","nbd_reads":{nbd_reads},"nbd_read_bytes":{nbd_read_bytes},"reads":{reads},"read_bytes":{read_bytes},"s3_fetches":{s3_fetches},"s3_bytes":{s3_bytes},"foyer_hits":{foyer_hits},"foyer_misses":{foyer_misses},"chunk_hits":{chunk_hits},"chunk_misses":{chunk_misses},"prefetch_enqueued":{prefetch_enqueued}}}"#
        ));
        tracing::info!(
            nbd_reads,
            nbd_read_bytes,
            reads,
            read_bytes,
            s3_fetches,
            s3_bytes,
            foyer_hits,
            foyer_misses,
            chunk_hits,
            chunk_misses,
            prefetch_enqueued,
            "io trace summary"
        );
    }

    fn write_line(&self, line: &str) -> std::io::Result<()> {
        let line = stamp_json_line(line);
        let mut w = self
            .writer
            .lock()
            .map_err(|_| std::io::Error::other("io log lock poisoned"))?;
        w.write_all(line.as_bytes())?;
        w.write_all(b"\n")?;
        w.flush()
    }
}

fn utc_timestamp_rfc3339() -> String {
    OffsetDateTime::now_utc()
        .format(&Rfc3339)
        .unwrap_or_else(|_| "unknown".into())
}

/// Prepend `"ts":"<RFC3339>",` to a JSON object line (must start with `{`).
fn stamp_json_line(line: &str) -> String {
    if let Some(rest) = line.strip_prefix('{') {
        let ts = utc_timestamp_rfc3339();
        format!(r#"{{"ts":"{ts}",{rest}"#)
    } else {
        line.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::stamp_json_line;

    #[test]
    fn stamp_json_line_prepends_ts() {
        let out = stamp_json_line(r#"{"kind":"read","offset":0}"#);
        assert!(out.starts_with(r#"{"ts":""#));
        assert!(out.contains(r#""kind":"read""#));
        assert!(out.ends_with(r#""offset":0}"#));
    }
}

pub struct ReadTimer {
    start: Instant,
}

impl ReadTimer {
    pub fn start() -> Self {
        Self {
            start: Instant::now(),
        }
    }

    pub fn elapsed_us(&self) -> u64 {
        self.start.elapsed().as_micros().min(u64::MAX as u128) as u64
    }
}
