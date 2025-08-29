#!/bin/bash -ex

. .world/build_config.sh

# don't build for 32-bit Windows for now
if [[ $Target == 'windows' && $Architecture == '32' ]]; then
  exit
fi

if [[ $Target == 'windows'  ]]; then
  RUST_TARGET="--target x86_64-pc-windows-gnu"
fi

cargo test $RUST_TARGET
cargo clippy --all-features --all-targets
cargo cbuild --prefix="$INSTALL" --libdir=lib $RUST_TARGET
