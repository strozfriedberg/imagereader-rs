# imagereader-rs CI

GitHub Actions workflow for lint, build/test, C API tests, and release binary
artifacts. Ported from the GitLab pipeline (`.gitlab-ci.yml`), mirroring the
Cedar CI layout.

## Workflow overview

- **File:** `.github/workflows/ci.yml`
- **Triggers:**
  - `pull_request` (all branches) — runs when a PR is opened or updated
  - `push` to `main` only — post-merge safety net; not `push` on all branches
    (avoids duplicate runs when a PR branch is pushed)
- **Concurrency:** `cancel-in-progress: true` — newer commits on the same ref
  cancel in-flight runs
- **Permissions:** `contents: read` (least privilege)

CI does **not** run on pushes to feature branches without an open PR. This
matches the GitLab `workflow:` rules, which ran one pipeline per change.

## Jobs

| Job | Timeout | What it runs |
|---|---|---|
| `lint` | 20 min | `cargo fmt --all -- --check` then `cargo clippy --workspace --all-features --all-targets --locked -- -D warnings` |
| `build-test` | 20 min | `cargo build --workspace --all-targets --all-features --locked` then `cargo test --workspace --all-features --locked --tests` |
| `ctest` | 20 min | `cargo install cargo-c` (cached) then `cargo ctest` in `vmdk`, `e01`, `rawdisk` |
| `build-artifact` | 40 min | `scripts/build-release.sh`; uploads `e01verify` and `diskimage-nbd` |

All four jobs run in parallel (no `needs`), as they did on GitLab.

For branch protection, require these check names (or the workflow as a whole):
`lint`, `build-test`, `ctest`, `build-artifact`.

### Why `ctest` is its own job

`cargo-c` works one crate at a time, so the C API is built and exercised per
crate. This is the only thing that links the capi surface as a real C library;
`cargo test --all-features` only covers the Rust side of it.

### Why `build-artifact` is separate from `build-test`

`lto = true` in the release profile, so the release build is slow and shares
almost nothing with the debug build in `build-test`. Merging them (as Cedar
does for its `build` + `test`) would serialize a fast job behind a slow one.

`scripts/build-release.sh` refuses to build a dirty tree so the embedded commit
always identifies the source; the CI checkout is clean, and `Swatinem/rust-cache`
restores only under gitignored paths.

## Rust toolchain

`rust-toolchain.toml` at the repo root is the **single source of truth** for the
pinned Rust channel (currently **1.97.1**). Bump the version there only — CI and
local dev both follow it. This replaces the GitLab `image: rust:1.97` pin.

### How the version flows in CI

All four jobs call the composite action
[`.github/actions/setup-rust`](actions/setup-rust/action.yml) after checkout:

```yaml
- uses: ./.github/actions/setup-rust
```

The `lint` job also passes extra components:

```yaml
- uses: ./.github/actions/setup-rust
  with:
    components: rustfmt, clippy
```

Inside `setup-rust`:

1. **Parse** — reads `channel` from `rust-toolchain.toml` via `sed`
2. **Install** — `dtolnay/rust-toolchain` with `toolchain:` set to the parsed channel

`dtolnay/rust-toolchain` does **not** read `rust-toolchain.toml` itself. It only
installs what you pass in `toolchain:` (or what its `@rev` implies). Because the
action is SHA-pinned for supply-chain security, the channel must be passed
explicitly — hence the parse step.

Unlike Cedar's, this composite has **no apt step**: nothing here uses bindgen, so
clang is not needed, and `openssl-sys` (pulled in by `rust-s3`) finds
`libssl-dev` and `pkg-config` preinstalled on the `ubuntu-24.04` image. This is
the same constraint that kept GitLab off `rust:*-slim`.

### Components (`rustfmt`, `clippy`)

`rust-toolchain.toml` lists `components` for **local** `rustup` installs.
`dtolnay/rust-toolchain` does **not** install components from that file — only the
channel. So the `lint` job must pass `components: rustfmt, clippy`; the other
three jobs need no extra components. The GitLab jobs ran
`rustup component add` inline for the same reason.

### Bumping the Rust version

1. Update `channel` in `rust-toolchain.toml`
2. Run `rustup toolchain install` locally and verify `cargo build` / `cargo test`
3. Push — CI picks up the new channel automatically (no workflow edits needed)

## Local development

### Lint (same as CI `lint` job)

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-features --all-targets --locked -- -D warnings
```

### Build and test (same as CI `build-test` job)

```bash
cargo build --workspace --all-targets --all-features --locked
cargo test --workspace --all-features --locked --tests
```

### C API tests (same as CI `ctest` job)

```bash
cargo install cargo-c --locked
for crate in vmdk e01 rawdisk; do (cd "$crate" && cargo ctest); done
```

### Release binaries (same as CI `build-artifact` job)

```bash
scripts/build-release.sh   # requires a clean tracked tree
```

## Caching

- **Rust:** [`Swatinem/rust-cache`](https://github.com/Swatinem/rust-cache) with
  default per-job keys (no `shared-key`), `cache-on-failure: true`
  - Each job caches `~/.cargo` and a pruned `target/` separately (`lint`,
    `build-test`, `ctest`, `build-artifact` build different profiles/metadata)
  - PRs restore the latest cache saved for that job on `main` (restores are
    branch-agnostic)
  - `CARGO_INCREMENTAL: 0` at workflow level (incremental artifacts are stripped
    by rust-cache)
- **`cargo-c`:** `cache-bin: true` on the `ctest` job keeps `cargo-ctest` and
  friends in `~/.cargo/bin` across runs; the install is guarded by
  `command -v cargo-ctest`. Replaces the GitLab `cargo-bin-cargo-c` cache key.
- **No sccache** — replaced sccache (GitLab, five separate `SCCACHE_DIR`s plus a
  cached sccache binary) with rust-cache; trades fine-grained compiler-wrapper
  reuse for simpler per-job dependency caching.

## Artifacts

- **Job:** `build-artifact`
- **Paths:** `target/release/e01verify`, `target/release/diskimage-nbd`
- **Retention:** 30 days (was 3 days on GitLab)
- **Download:** GitHub Actions run → job `build-artifact` → Artifacts section, or
  `gh run download`

Each binary reports the commit in `--version` (e.g. `0.1.1 (a1b2c3d)`), baked in
by `build.rs` from `$GIT_COMMIT`.

## Action pinning

Every `uses:` reference is a full 40-character commit SHA with the release tag in
a `#` comment. SHAs were taken from the Cedar workflow; resolve them yourself at
bump time:

```bash
gh api repos/actions/checkout/git/refs/tags/v7.0.1 --jq .object.sha
```

| Action | Version (comment) | SHA |
|---|---|---|
| `actions/checkout` | v7.0.1 | `3d3c42e5aac5ba805825da76410c181273ba90b1` |
| `dtolnay/rust-toolchain` | master | `6c977a6ca4077a0ceb28ffbe03f59d46e9ac8772` |
| `Swatinem/rust-cache` | v2.9.1 | `23869a5bd66c73db3c0ac40331f3206eb23791dc` |
| `actions/upload-artifact` | v7.0.1 | `043fb46d1a93c77aae656e7c1c64a875d1fc6a0a` |

`dtolnay/rust-toolchain` is referenced only inside
[`.github/actions/setup-rust`](actions/setup-rust/action.yml). It has no recent
release tags beyond `v1`; pin to the current `master` commit when bumping.

## Migration record

### GitLab → GitHub Actions mapping

| GitLab job | GitHub Actions job |
|---|---|
| `fmt` + `clippy` | `lint` (single job) |
| `test` | `build-test` (adds an explicit `cargo build --all-targets`) |
| `ctest` | `ctest` |
| `build` | `build-artifact` |

### Intentional differences from GitLab

- Merged `fmt` + `clippy` into one `lint` job
- sccache → `Swatinem/rust-cache`
- `ubuntu-24.04` + `rust-toolchain.toml` instead of the `rust:1.97` Docker image
  (via the `setup-rust` composite action)
- `--locked` added to the clippy invocation (GitLab omitted it)
- Artifact retention 30 days (was 3 days)
- No CI on branch pushes without an open PR (same effective behavior as the
  GitLab `workflow:` rules)

### Common failure modes

| Symptom | Likely cause | Fix |
|---|---|---|
| `openssl-sys` build failure | Missing SSL dev headers | Should not happen on `ubuntu-24.04`; add `libssl-dev pkg-config` to `setup-rust` if the image ever drops them |
| `build-release.sh` refuses to build | Dirty tracked tree | Should not happen in CI; locally, commit or stash first |
| Artifact upload: file not found | Release build produced nothing | Check `scripts/build-release.sh` output; `if-no-files-found: error` makes this loud |
| `cargo ctest` not found | `cargo-c` cache restored without the binary | The `command -v` guard reinstalls it; check the `cache-bin` step |
| rust-cache not restoring on PRs | Fork PR | Fork PRs cannot write cache; same-repo PRs restore base-branch cache per job |

## Post-cutover checklist

- [ ] Verify all four GHA jobs green on `main`
- [ ] Configure branch protection required checks if desired (`lint`,
      `build-test`, `ctest`, `build-artifact`)
- [ ] Delete `.gitlab-ci.yml` (post-cutover; `origin` is still the GitLab remote)
- [ ] Remove GitLab CI/CD variables if any were configured
- [ ] Confirm this document matches the workflow
