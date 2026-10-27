#!/usr/bin/env bash
# shellcheck disable=SC2016 # backticks in single quotes are Markdown
# Times warm scans of a generated tree by this checkout's dfdu and by the one in <base-dir>, on
# the same machine, and writes the result to bench.md and the job summary.
# Usage: bench-base.sh <base-dir> <work-dir> [entries]
set -euo pipefail

base=$1
work=$2
entries=${3:-200000}
summary=${GITHUB_STEP_SUMMARY:-/dev/stdout}

cargo build --release --locked -p dfdu -p dfdu-bench
cargo build --release --locked -p dfdu --manifest-path "$base/Cargo.toml"
target/release/dfdu-bench gen "$work/tree" --entries "$entries"
hyperfine -N --warmup 3 --runs 15 --export-markdown "$work/hyperfine.md" \
    -n base "$base/target/release/dfdu scan $work/tree" \
    -n head "target/release/dfdu scan $work/tree"
{
    echo '## Benchmark'
    echo
    echo "Warm \`dfdu scan\` of a generated $entries-entry tree, base against head on one runner."
    echo
    cat "$work/hyperfine.md"
} >bench.md
cat bench.md >>"$summary"
