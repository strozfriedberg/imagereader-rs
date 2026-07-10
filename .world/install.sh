#!/bin/bash -ex

. .world/build_config.sh

CRATES="vmdk e01 rawdisk"

if [[ $Target == 'windows'  ]]; then
  # --meson-paths ensures we produce lib<name>.dll.a instead of <name>.dll.a
  if [[ $Architecture == '32' ]]; then
    # only vmdk builds for 32-bit Windows
    CRATES="vmdk"
    RUST_OPTS="--target i686-pc-windows-gnu --meson-paths"
  else
    RUST_OPTS="--target x86_64-pc-windows-gnu --meson-paths"
  fi
fi

for crate in $CRATES; do
  pushd $crate
  cargo cinstall --prefix="$INSTALL" --libdir=lib $RUST_OPTS
  popd
done
