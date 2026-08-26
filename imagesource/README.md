# imagesource

`imagesource` is the shared infrastructure the disk-image readers in this
workspace are built on: byte sources, caching, S3 credential resolution,
existence probing, and I/O logging. It knows nothing about image formats. The
readers (currently [`e01`](../e01), [`vmdk`](../vmdk), and [`rawdisk`](../rawdisk)) supply the
format knowledge; `imagesource` supplies the bytes.

## The layers

```
  reader (e01 / vmdk / rawdisk)
      |  read_at_offset(image offset)
  Cache          ...  FoyerCache: hybrid memory/disk, block-granular
      |  miss -> fetch
  FetchLimiter   ...  bounds in-flight backing-store reads
      |
  BytesSource    ...  FileSource | S3Source, addressed by segment index
```

### `BytesSource`

The bottom of the stack: `read(beg, end) -> Vec<u8>` plus `end()`. There are two
implementations: `FileSource` and `S3Source`. `urlsource::source_for_url` picks
one from a path or URL, and `path_or_url_to_url` formats a plain path as
a `file://` URL so both cases flow through the same code.

Sources are registered into a cache by index (`SourceSlots`), because an image is
often several files -- E01 segments, VMDK extents, split raw segments. Note
that a slot may legitimately be empty (e.g., `vmdk` can skip an index when walking 
a malformed extent chain).

### `Cache`

`Cache` is the trait the readers hold: `read(idx, off, buf, trace)`, `end(idx)`,
`add_source(idx, src)`. Two implementations ship: `FoyerCache`, the real one, and
`DummyCache`, which passes straight through to the source for benchmarking.

`FoyerCache` wraps [foyer](https://github.com/foyer-rs/foyer) and separates two
sizes:

* **block size** (`cache_chunk_size`) -- the granularity at which blocks are
  stored and evicted. Small blocks let the cache hold exactly what is hot; a
  scattered 200 KB index read should not pin megabytes of junk. Memory capacity 
  is a byte budget divided by this, so changing it does not change the cache's 
  footprint.
* **fetch size** (`cache_fetch_size`) -- bytes pulled from the backing store per
  miss. Against S3 this is the knob that matters; S3 reads are high latency and
  the size of the read is almost irrelevant. When fetch size exceeds block size, 
  one GET fills the demanded block plus its aligned siblings as separate cache 
  entries, so eviction stays fine-grained and untouched siblings are dropped first. 
  Against a local file, leave it equal to the block size -- there the bytes are 
  *not* free, and a large fetch is wasted bandwidth on scattered reads.

The default (`DEFAULT_CACHE_FETCH_SIZE == DEFAULT_CACHE_CHUNK_SIZE`) leaves
coalescing off until it is asked for.

### `CacheMode` and the metadata tier

`CacheMode::SingleMemory` is a single memory-only cache which is the right approach for a
local file.

`CacheMode::DualHybrid` exists for cache-warming workflows against S3. A
filesystem walk touches metadata -- MFT, inodes, directory blocks -- over and over,
while content blocks stream past once.  `DualHybrid` gives metadata a dedicated 
protected tier alongside the content cache.

The two phases are separated by a signal. `regular_phase` starts
`false`, and the server flips it with `SIGUSR1` once the metadata-heavy stage is
done -- see [`diskimage-nbd`](../diskimage-nbd/README.md#cache-warming).

### `FetchLimiter`

A resource limit that bounds how many backing-store reads are in flight. Foyer
spawns a task per miss and will not stop at any particular number, and unbounded
concurrency costs memory and file descriptors.

It deliberately does *not* deduplicate concurrent fetches of the same block.
Foyer's `get_or_fetch` already coalesces them inside the same lock as the lookup 
and drives the winning fetch on its own task.

### `ExistsChecker`

Decides whether a candidate segment path exists, so a reader's segment discovery
can tell the end of a sequence from a hole. 

The contract is narrow: **`Ok(false)` means definitely absent**,
and it terminates a sequence. Therefore a probe that could not be answered -- a throttled
HEAD, an expired credential, an unreadable directory -- should return
`Err(ExistsError)`, not `Ok(false)`.

### `IoLog`

Append-only JSONL tracing, for workload analysis rather than for production use --
it is expensive enough to distort what it measures, so enable it as a diagnostic
only. `ReadTrace` collects per-read cache outcomes; `ReadTimer` times them.

## S3 credentials

Credentials are resolved via the AWS SDK Rust
[`DefaultCredentialsChain`](https://docs.rs/aws-config/latest/aws_config/default_provider/credentials/struct.DefaultCredentialsChain.html)
(profile files, environment variables, SSO, ECS, EC2 instance role).

**Resolution policy:** the chain is always attempted for `s3://` opens
(environment variables, `AWS_CONFIG_FILE`, `AWS_SHARED_CREDENTIALS_FILE`, or
`~/.aws/credentials` / `~/.aws/config`). If resolution fails and AWS auth is
expected, the open fails with an explicit error. If no auth is configured, the
reader falls back to anonymous access (public buckets).

**EC2 instance role:** works without local AWS config files via IMDS in the chain.

**Public buckets:** when no AWS config is present, the chain may probe IMDS before
falling back to anonymous (~1–5 s on first open). For faster anonymous access, set
`AWS_EC2_METADATA_DISABLED=true`.

**Refresh:** temporary credentials (SSO, STS) are re-provisioned via aws-config
before S3 reads when they expire. SSO profiles require a prior `aws sso login`.

**Region:** when `AWS_REGION` / `AWS_DEFAULT_REGION` are unset, the region from the
active AWS profile (including SSO profiles) is used. If no region is configured,
the bucket's location is queried before reading rather than assumed.

## Benchmarks

```sh
cargo bench -p imagesource
```

`benches/cache_bench.rs` exercises the cache layer.

### Copyright

Copyright 2025–2026, LevelBlue. `imagesource` is licensed under the Apache License, Version 2.0.
