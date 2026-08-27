#!/usr/bin/env bash
#
# Build everything shippable in release mode with the current commit embedded:
#
#   - every binary in the workspace (e01verify, e01bench, vmdkverify,
#     rawdiskverify, diskimage-nbd) into target/release/
#   - the C libraries and headers of the capi crates (vmdk, e01, rawdisk),
#     installed via cargo-c into target/release/dist/{lib,include}
#
#   - Refuses to build if any tracked file has uncommitted changes, so the
#     embedded hash always identifies the exact source. Untracked scratch files
#     (logs, caches) do not block the build but are listed.
#   - Passes the commit to the build via $GIT_COMMIT; binaries built with
#     buildinfo report it in `--version` (e.g. "0.1.1 (a1b2c3d)") and it is
#     greppable with `strings`. `-V` still prints the plain crate version.
#
# Requires cargo-c (`cargo install cargo-c --locked`).
#
# Usage: scripts/build-release.sh
set -euo pipefail

cd "$(git rev-parse --show-toplevel)"

# 1. Clean tree (tracked files only).
if ! git diff --quiet || ! git diff --cached --quiet; then
    echo "error: uncommitted changes to tracked files; commit or stash first:" >&2
    git status --short --untracked-files=no >&2
    exit 1
fi

untracked="$(git ls-files --others --exclude-standard)"
if [[ -n "$untracked" ]]; then
    echo "note: ignoring untracked files (not part of the build):" >&2
    sed 's/^/  /' <<<"$untracked" >&2
    echo >&2
fi

if ! command -v cargo-cinstall >/dev/null; then
    echo "error: cargo-c is not installed (cargo install cargo-c --locked)" >&2
    exit 1
fi

# 2. Capture the commit; build.rs reads $GIT_COMMIT and bakes it in.
commit="$(git rev-parse --short HEAD)"
export GIT_COMMIT="$commit"
echo "building release at commit ${commit}"

target_dir="${CARGO_TARGET_DIR:-target}"
dist="$(realpath -m "${target_dir}/release/dist")"

# 3. Release build (LTO is on for the release profile, so this is not quick).
cargo build --release --workspace --bins

# The same crate list .world uses.
Target="${Target:-}" Architecture="${Architecture:-}"
. .world/crates.sh
rm -rf "$dist"
for crate in $CRATES; do
    (cd "$crate" && cargo cinstall --release --features capi --prefix="$dist" --libdir=lib)
done

# 4. Report where everything landed.
echo
echo "built at commit ${commit}:"
for bin in e01verify e01bench vmdkverify rawdiskverify diskimage-nbd; do
    path="${target_dir}/release/${bin}"
    echo "  ${path}"
    # Only the buildinfo-aware binaries have a --version that names the commit.
    case "$bin" in
        e01verify|diskimage-nbd) "${path}" --version | sed 's/^/    /' ;;
    esac
done
echo "  ${dist}/"
find "$dist" -type f | sort | sed 's/^/    /'
