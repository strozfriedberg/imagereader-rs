#!/usr/bin/env bash
#
# Build the shippable binaries -- e01verify and diskimage-nbd -- in release mode
# with the current commit hash embedded.
#
#   - Refuses to build if any tracked file has uncommitted changes, so the
#     embedded hash always identifies the exact source. Untracked scratch files
#     (logs, caches) do not block the build but are listed.
#   - Passes the commit to the build via $GIT_COMMIT; each binary reports it in
#     `--version` (e.g. "0.1.1 (a1b2c3d)") and it is greppable with `strings`.
#     `-V` still prints the plain crate version.
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

# 2. Capture the commit; build.rs reads $GIT_COMMIT and bakes it in.
commit="$(git rev-parse --short HEAD)"
export GIT_COMMIT="$commit"
echo "building release binaries at commit ${commit}"

# 3. Release build (LTO is on for the release profile, so this is not quick).
cargo build --release -p e01-rs --bin e01verify
cargo build --release -p diskimage-nbd --bin diskimage-nbd

# 4. Report where they landed (respecting CARGO_TARGET_DIR).
target_dir="${CARGO_TARGET_DIR:-target}"
echo
echo "built at commit ${commit}:"
for bin in "${target_dir}/release/e01verify" "${target_dir}/release/diskimage-nbd"; do
    echo "  ${bin}"
    "${bin}" --version | sed 's/^/    /'
done
