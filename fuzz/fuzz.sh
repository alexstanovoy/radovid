#!/usr/bin/env bash
# Builds the fuzz targets with libFuzzer's coverage instrumentation on stable
# Rust (no sanitizer: that needs nightly) and runs one.
#
#   fuzz/fuzz.sh build                    build every target
#   fuzz/fuzz.sh run <target> [args...]   fuzz one target; extra arguments go to libFuzzer
#                                         (e.g. -max_total_time=3600)
#   fuzz/fuzz.sh campaign <seconds> [jobs] every target at once for that long, each in fork mode
#                                         with `jobs` processes (default 1), past crashes, hangs
#                                         and memory blowups, all of which land in artifacts/;
#                                         an input slower than FUZZ_TIMEOUT seconds (30) is a hang
#
# Each target keeps its corpus in fuzz/corpus/<target> and writes what it
# finds to fuzz/artifacts/<target>.
set -euo pipefail
here=$(cd "$(dirname "$0")" && pwd)
export RUSTFLAGS="-Cpasses=sancov-module -Cllvm-args=-sanitizer-coverage-level=4 \
-Cllvm-args=-sanitizer-coverage-inline-8bit-counters -Cllvm-args=-sanitizer-coverage-pc-table \
-Cllvm-args=-sanitizer-coverage-trace-compares --cfg fuzzing -Cdebug-assertions -Coverflow-checks"
# An explicit target keeps the flags off build scripts and proc macros.
triple=$(rustc -vV | sed -n 's/^host: //p')
build() {
    cargo build --manifest-path "$here/Cargo.toml" --release --bins --target "$triple"
}
case "${1:-}" in
build) build ;;
run)
    target=${2:?which target}
    shift 2
    build
    mkdir -p "$here/corpus/$target" "$here/artifacts/$target"
    exec "$here/target/$triple/release/$target" -artifact_prefix="$here/artifacts/$target/" \
        "$here/corpus/$target" "$@"
    ;;
campaign)
    seconds=${2:?how long}
    jobs=${3:-1}
    build
    for bin in "$here"/fuzz_targets/*.rs; do
        target=$(basename "$bin" .rs)
        mkdir -p "$here/corpus/$target" "$here/artifacts/$target"
        "$here/target/$triple/release/$target" -artifact_prefix="$here/artifacts/$target/" \
            -fork="$jobs" -ignore_crashes=1 -ignore_timeouts=1 -ignore_ooms=1 \
            -max_total_time="$seconds" -timeout="${FUZZ_TIMEOUT:-30}" -rss_limit_mb=2048 -malloc_limit_mb=2048 \
            "$here/corpus/$target" >"$here/artifacts/$target.log" 2>&1 &
    done
    wait
    find "$here/artifacts" -mindepth 2 -type f | sort
    ;;
*)
    sed -n '2,14p' "$0"
    exit 2
    ;;
esac
