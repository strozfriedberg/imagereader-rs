#!/bin/sh
set -e
cargo build --release
cp target/release/diskimage-nbd ../../velociraptor/deaddisk/resources/binaries/diskimage-nbd
