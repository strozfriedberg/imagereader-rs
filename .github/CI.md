# CI

`.github/workflows/ci.yml` is the source of truth for what runs; this file
covers what the workflow cannot say about itself.

## Shape

Four independent jobs -- `lint`, `build-test`, `ctest`, `build-artifact` --
on every pull request and on pushes to `main`. Feature-branch pushes without a
PR do not run CI. For branch protection, require those four check names.

The jobs are independent on purpose:

* `ctest` is separate because cargo-c works one crate at a time, and it is the
  only thing that links the `capi` surface as a real C library;
  `cargo test --all-features` only covers the Rust side of it.
* `build-artifact` is separate from `build-test` because `lto = true` in the
  release profile makes the release build slow and it shares almost nothing
  with the debug build. Its 40-minute timeout (against 20 for the others)
  reflects that; `ctest` gets 40 as well because it builds each crate afresh.

## Toolchain

`rust-toolchain.toml` pins the channel. `dtolnay/rust-toolchain` does not read
that file -- it installs only what it is passed -- so the
[`setup-rust`](actions/setup-rust/action.yml) composite parses `channel` out of
it and passes it explicitly. Likewise it does not install the `components`
listed there, so the `lint` job passes `rustfmt, clippy` itself.

To bump Rust: change `channel` in `rust-toolchain.toml`, build and test
locally, push. No workflow edits are needed.

`cargo-c` is installed with `cargo install cargo-c --locked` (unpinned, cached
via `cache-bin`) in both CI and `.world/setup.sh`.

## Reproducing a job locally

```bash
cargo fmt --all -- --check                                                   # lint
cargo clippy --workspace --all-features --all-targets --locked -- -D warnings
cargo build --workspace --all-targets --all-features --locked                # build-test
cargo test --workspace --all-features --locked --tests
for crate in vmdk e01 rawdisk; do (cd "$crate" && cargo ctest); done         # ctest
scripts/build-release.sh                                                     # build-artifact
```

`build-release.sh` refuses to build a dirty tree, so the embedded commit always
identifies the source. The CI checkout is clean and `Swatinem/rust-cache`
restores only under gitignored paths, so this never trips in CI; locally,
commit or stash first.

## Caching

`Swatinem/rust-cache` with default per-job keys, since the four jobs build
different profiles. PRs restore the latest cache saved for the job on `main`;
fork PRs cannot write cache. `CARGO_INCREMENTAL=0` because rust-cache strips
incremental artifacts anyway.

## Action pinning

Every `uses:` is a full commit SHA with the release tag in a comment. When
bumping, resolve the SHA yourself:

```bash
gh api repos/actions/checkout/git/refs/tags/v7.0.1 --jq .object.sha
```

`dtolnay/rust-toolchain` has no release tags beyond `v1`; pin to the current
`master` commit.
