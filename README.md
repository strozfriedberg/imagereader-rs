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

E01 (`.e01`), VMDK (`.vmdk`), and raw/dd (`.raw`, `.dd`, `.img`, or a numbered
segment such as `disk.001`). The format is chosen by the path's extension.

### Split raw images

A raw image split across numbered segments is opened by naming any one of them:

```
diskimage-nbd /images/disk.001 --unix /tmp/nbd.sock
```

Segments must be numbered with a `.` followed by at least two digits, zero-padded
to a fixed width (`disk.001`, `disk.dd.01`). The width is taken from the path you
name, so `.001` pairs with `.002` but never with `.02`. Sequences may start at
`000` or `001`.

Unpadded numbering is deliberately not recognized. `img.1` and `img.2` are far
more often two unrelated images than one split one, and no rule based on names
alone can tell those apart -- guessing wrong would serve `img.2`'s bytes as the
tail of `img.1` with no error at all. Files such as `backup.2024` are left alone
for the same reason: consecutive, same-width, but unpadded, so they open as
themselves rather than as a sequence with a missing start.

A hole in the sequence makes opening fail, naming the missing file. A split
image with a gap would otherwise read as a valid but short image, which parses
and mounts and looks correct until something reads past the gap.

That check is not total, and the limit is worth knowing. A segment missing
between the start of the sequence and the one you name is always caught,
whatever the size of the hole. Past the segment you name, only a one-segment
hole is caught: a gap two or more segments wide cannot be distinguished from
the end of the sequence, and the image opens short. Naming the LAST segment
rather than the first therefore gives the strongest check, because every
segment then falls below the one you named.

`split(1)`'s alphabetic output (`xaa`, `xab`) is not recognized.
