# diskimage-nbd

`diskimage-nbd` serves a forensic disk image over NBD, so anything that speaks
NBD — `nbd-client`, `qemu-nbd`'s client side, a loop-mounted filesystem, a
carving tool — can read an E01, VMDK, or raw image as an ordinary block device.
The export is **read-only**, always.

The point is images that are not local. An `s3://` URL is served exactly like a
local path, with [`imagesource`](../imagesource)'s cache absorbing the latency, so
a filesystem walk over an image sitting in a bucket is a practical thing to do
rather than a thought experiment.

```sh
diskimage-nbd /images/disk.E01 --unix /tmp/nbd.sock
nbd-client -u -N "" /tmp/nbd.sock /dev/nbd0
mount -o ro,norecovery /dev/nbd0p2 /mnt/evidence
```

## Usage

For local use prefer a **Unix socket**, which avoids TCP overhead:

```sh
diskimage-nbd --unix /tmp/nbd.sock /path/to/disk.E01
nbd-client -u -N "" /tmp/nbd.sock /dev/nbd0
```

TCP is also supported, on the default NBD port **10809**:

```sh
diskimage-nbd --listen 127.0.0.1:10809 /path/to/disk.E01
nbd-client -N "" -R 10809 <server> /dev/nbd0
```

`--listen` and `--unix` conflict; give one.

## Image formats

The format is chosen by the suffix after the final `.` of the last path
component:

| suffix | format | reader |
| ------ | ------ | ------ |
| `.e01` | E01 / EWF | [`e01-rs`](../e01) |
| `.vmdk` | VMDK | [`vmdk-rs`](../vmdk) |
| `.raw`, `.dd`, `.img` | raw (dd) | [`rawdisk-rs`](../rawdisk) |
| all digits (`.001`, `.150`) | split raw segment | [`rawdisk-rs`](../rawdisk) |

Matching is case-insensitive and works on `s3://` URLs. Note that it is *not*
`Path::extension()`, which reports `None` for a leading-dot filename: `/img/.001`
is a segment like any other and the readers open it without complaint, so the CLI
must not refuse it.

The numeric rule is deliberately broad — any all-digit suffix is raw. Narrowing it
to something more segment-shaped would reject `disk.150`, an ordinary way to name
the segment you happen to have. A false positive costs nothing: raw means "serve
these bytes", so an unrecognized file is served as-is rather than misparsed.

Naming **any** segment of a split raw image opens the whole image; which segment
you name affects how reliably a missing one is detected, so see
[`rawdisk-rs`](../rawdisk/README.md#incomplete-sequences).

`-i` / `--ignore-checksums` zeroes chunks that fail their checksum instead of
erroring. E01 only; VMDK and raw have no per-chunk checksums, so it silently has
no effect there.

## Tuning

The flags that matter most are the two cache sizes, and they are not the same
knob:

* `--cache-chunk-size` (default 1 MiB) — the block size the cache stores and
  evicts at. Memory capacity is a byte budget divided by this, so it does not
  change the footprint.
* `--cache-fetch-size` (defaults to the chunk size) — bytes fetched from the
  backing store per miss. **Against S3 this is the important one.** A range GET is
  almost all fixed latency: 1 MiB measured at 221 ms, 16 MiB at 153 ms. An NTFS
  metadata walk that needed 1,192 fetches at 1 MiB needs 447 at 8 MiB, each no
  slower. Against a local file, leave it unset.

Also available: `--cache-mem-mib` (default 1024), `--cache-dir` for foyer's
on-disk cache (a random subdirectory is created under it; defaults to the OS temp
directory), `--readahead` in blocks for sequential reads, and `--s3-concurrency`
(default 8) for in-flight range GETs.

### Cache warming

`--metadata-cache` enables a two-tier cache: a protected metadata tier alongside
the content cache. A filesystem walk touches the MFT, inodes and directory blocks
repeatedly while content streams past once, and a single shared cache lets the
stream evict the metadata that is about to be needed again.

The server starts in the metadata phase and switches when told to, not on a
heuristic:

```sh
diskimage-nbd s3://bucket/case/disk.E01 --unix /tmp/nbd.sock --metadata-cache &
# ... run the metadata-heavy stage (fls, fsstat, a directory walk) ...
kill -USR1 %1        # switch to the regular content cache
# ... now stream files out ...
```

Sizes: `--metadata-cache-mem-mib` (256), `--metadata-cache-disk-mib` (4096),
`--content-cache-disk-mib` (4096).

The SIGUSR1 handler is registered before any blocking open, because the default
disposition for SIGUSR1 is to terminate the process.

### Tracing

Two logs, deliberately separate, both JSONL and both appended to:

* `--io-log` — reads at the NBD protocol level. What the client asked for.
* `--cache-trace-log` — the reader's own per-read cache hit/miss trace (the foyer
  tier, plus e01's secondary decoded-chunk cache). What that cost.

Reads issued while the image is opening are suppressed so they do not swamp the
served workload. Both logs are expensive enough to distort what they measure —
diagnostics, not production settings.

`RUST_LOG` controls tracing output; foyer is filtered to `warn` by default because
it is noisy below that.

## Protocol

A minimal fixed new-style NBD server for read-only exports. Compared to the `nbd`
crate's server it avoids flushing after every transmission command and reads
export data through the reader's `read_at_offset` directly.

Handshake options: `NBD_OPT_EXPORT_NAME`, `NBD_OPT_GO`, `NBD_OPT_INFO`,
`NBD_OPT_LIST`, `NBD_OPT_ABORT`. Transmission commands: `NBD_CMD_READ`,
`NBD_CMD_DISC`, and `NBD_CMD_FLUSH` (a no-op on a read-only export).
`NBD_CMD_WRITE` is rejected with EPERM (its payload is still drained, or request framing would desync); unknown commands get ENOSYS. The export advertises `NBD_FLAG_READ_ONLY`; reads
are capped at 32 MiB per request.

Any reader implementing the `NbdImage` trait — `size()` and `read_at_offset()` —
can be served, which is the seam the three format adapters plug into.

### Connections

One client at a time. The image reader is behind a mutex, so a session holds it
for its lifetime; at most one further connection is allowed to wait, covering a
client reconnecting while its old session is still tearing down. Beyond that,
connections are dropped at accept time rather than piling up as blocked threads.
When the active sessions disconnect, new connections are accepted again.

## Building

```sh
cargo build --release --bin diskimage-nbd
```

`scripts/build-release.sh` builds this along with everything else in the
workspace. It refuses to build with uncommitted tracked changes and embeds the
commit, so a binary always identifies its exact source:

```
$ diskimage-nbd --version
diskimage-nbd 0.1.0 (a1b2c3d)   # --version: crate version + commit
$ diskimage-nbd -V
diskimage-nbd 0.1.0             # -V: plain crate version
```

## S3 credentials

Image paths may be `s3://bucket/key` URLs. Credential and region resolution is
shared across the workspace — see
[`imagesource`](../imagesource/README.md#s3-credentials).

### Copyright

Copyright 2025–2026, LevelBlue. `diskimage-nbd` is licensed under the Apache License, Version 2.0.
