#!/usr/bin/env bash
#
# Run every criterion bench target in the workspace, passing arguments straight
# through to criterion.
#
# Why this exists rather than `cargo bench`:
#
#   * `cargo bench` (with or without --benches) also benchmarks every target
#     with `bench = true`, which is the default for lib and bin targets. Cargo
#     runs those through the libtest harness, which does not understand
#     criterion's arguments and dies with "Unrecognized option: 'save-baseline'".
#     Naming each --bench target explicitly is the only selection that runs
#     criterion and nothing else.
#
#   * Criterion resolves its data directory relative to the cwd, so running the
#     benches from each crate directory would scatter baselines into
#     e01/target/criterion, vmdk/target/criterion, ... and nothing would be
#     comparable. CRITERION_HOME pins them all to one place.
#
# Usage:
#   scripts/bench.sh [-p CPUS] [CRITERION_ARGS...]
#
#   -p CPUS   pin the run to these CPUs via taskset, e.g. -p 2,3
#
# Examples:
#   scripts/bench.sh -p 2,3 --save-baseline main    # record a reference point
#   scripts/bench.sh -p 2,3 --baseline main         # compare against it
#   scripts/bench.sh -p 2,3 --baseline main 'warm cache'   # ...just those groups
#
set -euo pipefail

PIN_CPUS=""
# Hand-rolled rather than getopts: everything after our own flags is a criterion
# argument, and those start with `-` too (--save-baseline, --baseline), which
# getopts would try to interpret as ours. Stop at the first thing we don't own.
while [[ $# -gt 0 ]]; do
  case "$1" in
    -p) PIN_CPUS="${2:?-p needs a CPU list, e.g. -p 2,3}"; shift 2 ;;
    -h) sed -n '2,28p' "$0"; exit 0 ;;
    --) shift; break ;;
    *) break ;;
  esac
done

REPO_ROOT="$(git rev-parse --show-toplevel)"
cd "$REPO_ROOT"

export CRITERION_HOME="${CRITERION_HOME:-$REPO_ROOT/.bench-ab/criterion}"
TARGET_DIR="${CARGO_TARGET_DIR:-$REPO_ROOT/target}"

RUNNER=()
if [[ -n "$PIN_CPUS" ]]; then
  command -v taskset >/dev/null || { echo "error: taskset not found" >&2; exit 1; }
  RUNNER=(taskset -c "$PIN_CPUS")
fi

echo "==> tree           : $(git rev-parse --short HEAD)$([[ -n "$(git status --porcelain)" ]] && echo ' + uncommitted')"
echo "==> criterion home : $CRITERION_HOME"
[[ -n "$PIN_CPUS" ]] && echo "==> pinned to CPUs : $PIN_CPUS"
echo "==> criterion args : $*"

while IFS= read -r bench; do
  crate_dir="$(dirname "$(dirname "$bench")")"   # e01/benches/x.rs -> e01
  bench_name="$(basename "$bench" .rs)"

  echo
  echo "--- $crate_dir :: $bench_name"
  (
    cd "$REPO_ROOT/$crate_dir"
    CARGO_TARGET_DIR="$TARGET_DIR" \
      "${RUNNER[@]}" cargo bench --bench "$bench_name" -- "$@"
  )
done < <(git ls-files '*/benches/*.rs')
