# imagereader-rs

Cargo workspace of disk-image readers with local-file and direct-from-S3
support, backed by a shared foyer-based caching layer.

- [`imagesource`](imagesource/README.md) — shared infrastructure: byte sources
  (file, S3), hybrid memory/disk caching with a protected metadata tier, AWS
  credential resolution, and I/O logging.
- [`vmdk`](vmdk/README.md) — VMDK reader (`vmdk-rs`, C API prefix `vmdk_*`).
- [`e01`](e01/README.md) — EWF/E01 reader (`e01-rs`, C API prefix `e01_*`).
- [`rawdisk`](rawdisk/README.md) — raw (dd) image reader (`rawdisk-rs`, C API
  prefix `rawdisk_*`).
- [`diskimage-nbd`](diskimage-nbd/README.md) — read-only NBD server for all
  three formats.

Each reader crate builds a C library via cargo-c (`cargo cinstall` from the
crate directory).

## Building

`rust-toolchain.toml` pins the Rust channel; rustup and CI both follow it.

`scripts/build-release.sh` builds every binary and, through cargo-c, every C
library in release mode with the commit embedded; the libraries and headers
land in `target/release/dist/`. See
[`diskimage-nbd`](diskimage-nbd/README.md#building) for how a binary reports
its commit. [`.github/CI.md`](.github/CI.md) describes the CI workflow.

### Copyright

Copyright 2025–2026, LevelBlue, LLC. `imagereader-rs` is licensed under the Apache License, Version 2.0.
