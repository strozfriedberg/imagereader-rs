#!/bin/bash -ex

. .world/build_config.sh
. .world/crates.sh

cargo clippy --workspace --all-features --all-targets

# cargo-c runs per C-API crate; each installs its own lib + header.
if [[ $Target == 'windows' ]]; then
  if [[ $Architecture == '32' ]]; then
    RUST_OPTS="--target i686-pc-windows-gnu --config target.i686-pc-windows-gnu.runner='wine' --meson-paths"
  else
    RUST_OPTS="--target x86_64-pc-windows-gnu --config target.x86_64-pc-windows-gnu.runner='wine' --meson-paths"
  fi
fi

for crate in $CRATES; do
  pushd $crate
  cargo ctest --prefix="$INSTALL" --libdir=lib $RUST_OPTS
  popd
done
