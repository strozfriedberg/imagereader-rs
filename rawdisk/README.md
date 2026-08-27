# rawdisk-rs

`rawdisk-rs` is a Rust library to read data from raw (dd) disk images, whether
held in a single file or split across numbered segments. Images may be read from
the local filesystem or directly from S3.

A raw image has no container and no metadata: the bytes are the image. The
reader is therefore very straightforward -- an offset maps to the same offset in the backing
store -- and the work it does is elsewhere: discovering the full set of segments,
mapping image offsets across segment boundaries, and reading through the shared
[`imagesource`](../imagesource) cache so that S3-backed images are usable.

### Supported inputs

* single-file images (`.raw`, `.dd`, `.img`)
* split images with numeric segments (`disk.001`, `disk.002`, ...)
* local paths and `s3://bucket/key` URLs

## Split images

A split raw image is a plain concatenation. Segments are
often not uniform, since the last one is short and some tools emit a short one in
the middle. `spans::SegmentMap` stores cumulative starts and binary searches
them to determine the segment for an offset.

### Naming a segment

Opening any one segment opens the whole image. Discovery (`seg_path.rs`) ignores
the number you named for the purposes of finding the sequence: it probes for a
start at `.000` and then `.001`, walks upward until a name is absent, and returns
the run it found.

Segments must be numbered with a `.` followed by at least two digits, zero-padded
to a fixed width (`disk.001`, `disk.dd.01`). The width is taken from the path you
name, so `.001` pairs with `.002` but not with `.02`.

### Incomplete sequences

There is no mode in which a gap is tolerated and skipped over: when discovery
detects one, the open fails with `missing image segment: <path>`, naming the
lowest segment it believes is absent. Gap detection capability depends on which 
segment you name:

* A gap of any width _below_ the named segment is always detected: the walk up
  from the start must reach the segment you named.
* A gap of exactly one segment _above_ the named segment is detected: discovery
  looks one name past the first absent one.
* A gap of two or more segments _above_ the named segment is **not** detected.
  The sequence ends at the gap and the image opens truncated.

So the most robust choice is to name the last segment if you know which it is;
everything below it is then checked.

### Usage example

Read from a raw image in Rust:

```rust
    use rawdisk::rawdisk_reader::RawdiskReader;

    // Any segment of a split image opens the whole image.
    let reader = RawdiskReader::open("/images/disk.001").unwrap();

    let mut buf: Vec<u8> = vec![0; 1048576];
    let mut offset = 0;
    while offset < reader.image_size {
        let read = reader.read_at_offset(offset, &mut buf).unwrap();
        if read == 0 {
            break;
        }

        // do something with buf[..read]

        offset += read as u64;
    }
```

Caching, readahead, S3 concurrency and I/O logging are configured through
`RawdiskReaderOptions` with `RawdiskReader::open_with_options`. `RawdiskReader::open`
uses the defaults, which are a memory-only cache sized by
`DEFAULT_CACHE_MEM_MIB`.

Read from a raw image in C:

```c
    #include "rawdisk.h"

    RawdiskError* err = nullptr;
    RawdiskHandle* handle = rawdisk_open(image_path, &err);
    if (err) {
        printf("%s\n", err->message);
        rawdisk_free_error(err);
        return;
    }

    char buf[4096];
    uint64_t offset = 0;
    while (offset < handle->image_size) {
        size_t r = rawdisk_read(handle, offset, buf, sizeof(buf), &err);
        if (err) {
            printf("%s\n", err->message);
            rawdisk_free_error(err);
            break;
        }

        // do something with buf[..r]

        offset += r;
    }

    rawdisk_close(handle);
```

The C library is built with cargo-c (`cargo cinstall` from this directory), which
enables the `capi` feature. Every entry point catches panics and reports them
through `err`, since a panic unwinding across the `extern "C"` boundary would
abort the caller's process.

## `rawdiskverify`

Hashes one or more images with SHA-1, reporting progress to stderr:

```sh
cargo run --release -p rawdisk-rs --bin rawdiskverify -- /images/disk.001 /images/other.raw
```

To serve a raw image over NBD, see [`diskimage-nbd`](../diskimage-nbd).

## S3 credentials

Paths may be `s3://bucket/key` URLs. Credential and region resolution is shared
across every reader in this workspace -- see
[`imagesource`](../imagesource/README.md#s3-credentials).

### Copyright

Copyright 2025–2026, LevelBlue. `rawdisk-rs` is licensed under the Apache License, Version 2.0.
