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

## Supported image formats

### Split raw images

A raw image split across numbered segments is opened by naming any one of them:

```
diskimage-nbd /images/disk.001 --unix /tmp/nbd.sock
```

Segments must be numbered with a `.` followed by digits (`disk.001`, `disk.dd.1`).
The digit width is taken from the path you name, so `.001` pairs with `.002` but
never with `.02`. Sequences may start at `000` or `001`.

If a segment in the middle is missing, opening fails and names it. This is
deliberate: a split image with a hole would otherwise read as a valid but short
image, which parses and mounts and looks correct until something reads past the
gap.

`split(1)`'s alphabetic output (`xaa`, `xab`) is not recognised.
