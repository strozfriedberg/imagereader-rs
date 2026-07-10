# imagereader-rs

Cargo workspace of disk-image readers with local-file and direct-from-S3
support, backed by a shared foyer-based caching layer.

- `imagesource` — shared infrastructure: byte sources (file, S3), hybrid
  memory/disk caching with a protected metadata tier, AWS credential
  resolution, and I/O logging.
- `vmdk` — VMDK reader (`vmdk-rs`, C API prefix `vmdk_*`).
- `e01` — EWF/E01 reader (`e01-rs`, C API prefix `e01_*`).
- `rawdisk` — raw (dd) image reader (`rawdisk-rs`, C API prefix `rawdisk_*`).

Each reader crate builds a C library via cargo-c (`cargo cinstall` from the
crate directory); this is how make_world consumes them.
