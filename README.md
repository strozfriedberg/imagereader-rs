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

## Release binaries

`scripts/build-release.sh` builds the shippable binaries — `e01verify` and
`diskimage-nbd` (the NBD server) — in release mode. It refuses to build when any
tracked file has uncommitted changes and embeds the commit it built from, so a
binary always identifies its exact source. Each binary reports that commit via
its long version string (and it is greppable with `strings`):

```
$ diskimage-nbd --version
diskimage-nbd 0.1.0 (a1b2c3d)   # --version: crate version + commit
$ diskimage-nbd -V
diskimage-nbd 0.1.0             # -V: plain crate version
```
