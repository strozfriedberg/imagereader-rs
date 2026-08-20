# e01-rs

`e01-rs` is a Rust library to read data from Expert Witness Format (E01) files.
This project is in active development and should be considered beta quality, with no known issues.

# [Expert Witness Compression Format (EWF)](https://github.com/libyal/libewf/blob/main/documentation/Expert%20Witness%20Compression%20Format%20(EWF).asciidoc)

### Supported file formats

* EWF
* EWF-E01
* EWF-S01

### Supported features

* multiple segments (files)
* chunk decompression (zlib)
* checking all checksums

## TODO

* [EWF2](https://github.com/libyal/libewf/blob/main/documentation/Expert%20Witness%20Compression%20Format%202%20(EWF2).asciidoc)

Sample of usage (library):

```
    use e01::e01_reader::E01Reader;

    fn read_e01(e01_path: &str) {
        let e01_reader = E01Reader::open(&e01_path).unwrap();

        let mut buf: Vec<u8> = vec![0; 1048576];
        let mut offset = 0;
        while offset < e01_reader.image_size {
            let read = e01_reader.read_at_offset(offset, &mut buf).unwrap();
            if read == 0 {
                break;
            }

            // process buf[..read]

            offset += read;
        }
    }

```

## `e01-nbd` — NBD server (qemu-style)

The `e01-nbd` binary serves a forensic image using the fixed new-style NBD handshake, similar to `qemu-nbd`. The export is **read-only**.

For local use, prefer a **Unix socket** (avoids TCP overhead):

```sh
cargo build --release --bin e01-nbd
./target/release/e01-nbd --unix /tmp/e01-nbd.sock /path/to/disk.E01
nbd-client -u -N "" /tmp/e01-nbd.sock /dev/nbd0
```

TCP is also supported (default port **10809**):

```sh
./target/release/e01-nbd --listen 127.0.0.1:10809 /path/to/disk.E01
nbd-client -N "" -R 10809 <server> /dev/nbd0
```

Optional: `-i` / `--ignore-checksums` (same meaning as for e01verify).

## S3 credentials

Segment paths may use `s3://bucket/key` URLs. Credentials are resolved via the
AWS SDK Rust [`DefaultCredentialsChain`](https://docs.rs/aws-config/latest/aws_config/default_provider/credentials/struct.DefaultCredentialsChain.html)
(profile files, environment variables, SSO, ECS, EC2 instance role).

**Resolution policy:** the chain is always attempted for `s3://` opens. If
resolution fails and AWS auth is expected (environment variables, `AWS_CONFIG_FILE`,
`AWS_SHARED_CREDENTIALS_FILE`, or `~/.aws/credentials` / `~/.aws/config`), open
fails with an explicit error. If no auth is configured, the reader falls back to
anonymous access (public buckets).

**Behavior change:** a default profile in `~/.aws/credentials` may be used
without setting `AWS_PROFILE` or other env vars (previously anonymous-only when
env vars were unset).

**EC2 instance role:** works without local AWS config files via IMDS in the chain.

**Public buckets:** when no AWS config is present, the chain may probe
IMDS before falling back to anonymous (~1–5s on first open). For faster anonymous
access, set `AWS_EC2_METADATA_DISABLED=true`.

**Refresh:** temporary credentials (SSO, STS) are re-provisioned via aws-config
before S3 reads when they expire. SSO profiles require a prior `aws sso login`.

When `AWS_REGION` / `AWS_DEFAULT_REGION` are unset, the region from the active
AWS profile (including SSO profiles) is used. If no region is configured, the
library queries the bucket location before reading rather than assuming one.


### Copyright

Copyright 2025, LevelBlue. `e01-rs` is licensed under the Apache License, Version 2.0.
