#!/usr/bin/env bash
#
# Compile and run the C API smoke tests (<crate>/c_api/test.c) against the
# libraries and headers cargo-c actually installs.
#
# `cargo test --all-features` exercises the capi modules from Rust, and
# `cargo ctest` builds the cdylib, but neither compiles a line of C against the
# generated headers. This does, in both C99 and C++17, so a header that only
# works from one of the two -- a missing `extern "C"` guard, say -- fails here.
#
# `--debug`, because cargo-c defaults to release: this checks an ABI, and the
# release profile's `lto = true` buys nothing for that while costing minutes per
# crate. It also shares artifacts with the `cargo ctest` run alongside it in CI.
#
# Requires cargo-c (`cargo install cargo-c --locked`) and a C and C++ compiler.
#
# Usage: scripts/ctest-capi.sh [crate...]   (default: the crates in .world/crates.sh)
set -euo pipefail

cd "$(git rev-parse --show-toplevel)"

if ! command -v cargo-cinstall >/dev/null; then
    echo "error: cargo-c is not installed (cargo install cargo-c --locked)" >&2
    exit 1
fi

cc_bin="${CC:-cc}"
cxx_bin="${CXX:-c++}"
for compiler in "$cc_bin" "$cxx_bin"; do
    if ! command -v "$compiler" >/dev/null; then
        echo "error: no C/C++ compiler: ${compiler} not found" >&2
        exit 1
    fi
done

# The image each crate's test opens. Kept small on purpose; the test asserts
# only what holds for any valid image, so these can be swapped freely.
fixture_for() {
    case "$1" in
        e01)     echo data/image.E01 ;;
        vmdk)    echo data/monolithicSparse.vmdk ;;
        rawdisk) echo data/patterned_4mib.raw ;;
        *)       return 1 ;;
    esac
}

if (( $# )); then
    crates=("$@")
else
    # The same crate list .world and build-release.sh use.
    Target="${Target:-}" Architecture="${Architecture:-}"
    . .world/crates.sh
    read -r -a crates <<<"$CRATES"
fi

target_dir="${CARGO_TARGET_DIR:-target}"
root="$(realpath -m "${target_dir}/ctest-capi")"
rm -rf "$root"

for crate in "${crates[@]}"; do
    echo "== ${crate}"

    fixture="$(fixture_for "$crate")" || {
        echo "error: no fixture registered for crate '${crate}'" >&2
        exit 1
    }
    if [[ ! -e "${crate}/${fixture}" ]]; then
        echo "error: missing fixture ${crate}/${fixture}" >&2
        exit 1
    fi

    # One prefix per crate, so the header and library below are unambiguous.
    prefix="${root}/${crate}"
    (cd "$crate" && cargo cinstall --debug --features capi --prefix="$prefix" --libdir=lib)

    # Find what cargo-c installed rather than predicting it: the header lives in
    # a subdirectory named after the library, which is not the crate name.
    header="$(find "${prefix}/include" -name '*.h' -type f | head -n1)"
    if [[ -z "$header" ]]; then
        echo "error: cargo-c installed no header under ${prefix}/include" >&2
        exit 1
    fi
    incdir="$(dirname "$header")"

    soname="$(find "${prefix}/lib" -maxdepth 1 -name 'lib*.so' | head -n1)"
    if [[ -z "$soname" ]]; then
        echo "error: cargo-c installed no shared library under ${prefix}/lib" >&2
        exit 1
    fi
    libname="$(basename "$soname")"
    libname="${libname#lib}"
    libname="${libname%.so}"

    warnings=(-Wall -Wextra -Werror)

    # The same source twice: once as C, once as C++. `-x c++` is what makes the
    # C++ compiler treat a .c file as C++ rather than falling back to C.
    "$cc_bin" -std=c99 "${warnings[@]}" -I"$incdir" \
        "${crate}/c_api/test.c" -o "${prefix}/test-c" \
        -L"${prefix}/lib" -l"$libname"
    "$cxx_bin" -std=c++17 "${warnings[@]}" -I"$incdir" \
        -x c++ "${crate}/c_api/test.c" -o "${prefix}/test-cxx" \
        -L"${prefix}/lib" -l"$libname"

    for variant in c cxx; do
        echo "-- ${crate} test-${variant}"
        LD_LIBRARY_PATH="${prefix}/lib${LD_LIBRARY_PATH:+:${LD_LIBRARY_PATH}}" \
            "${prefix}/test-${variant}" "${crate}/${fixture}"
    done
done

echo
echo "C API tests passed for: ${crates[*]}"
