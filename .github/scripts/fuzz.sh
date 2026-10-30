#!/usr/bin/env bash
# shellcheck disable=SC2016 # backticks in single quotes are Markdown
# Runs every fuzz target for <seconds> each from its corpus, seeds and regressions, going on
# after a crash so that one run reports every failing target. New inputs go to fuzz/corpus,
# crashes to fuzz/artifacts; the corpus sizes go to the job summary.
# Usage: fuzz.sh <toolchain> <seconds>
set -uo pipefail

toolchain=$1
seconds=$2
summary=${GITHUB_STEP_SUMMARY:-/dev/stdout}
cd "$(dirname "$0")/../../fuzz" || exit 1

targets=$(cargo "+$toolchain" fuzz list) || exit 1
# cargo-fuzz defaults to the triple it was built for, and a prebuilt binary may be musl,
# which the sanitizers reject; build for the toolchain's host instead.
host=$(rustc "+$toolchain" -vV | sed -n 's/^host: //p')
# A target that does not build is not a crash: say so, and run none.
if ! cargo "+$toolchain" fuzz build --target "$host"; then
    echo "::error::The fuzz targets do not build"
    exit 1
fi
failed=()
rows=()
for t in $targets; do
    mkdir -p "corpus/$t"
    dirs=("corpus/$t")
    for d in "seeds/$t" "regressions/$t"; do
        if [[ -d $d ]]; then dirs+=("$d"); fi
    done
    before=$(find "corpus/$t" -type f | wc -l)
    result=ok
    if ! cargo "+$toolchain" fuzz run --target "$host" "$t" "${dirs[@]}" -- \
        -max_total_time="$seconds" -timeout=10 -rss_limit_mb=2048; then
        failed+=("$t")
        result=failed
    fi
    after=$(find "corpus/$t" -type f | wc -l)
    rows+=("| \`$t\` | $result | $before | $after |")
done

{
    echo "## Fuzzing, ${seconds} s per target"
    echo
    echo '| Target | Result | Corpus before | Corpus after |'
    echo '|---|---|---|---|'
    printf '%s\n' "${rows[@]}"
} >>"$summary"

if ((${#failed[@]})); then
    echo "::error::Fuzz targets failed: ${failed[*]} (inputs in the fuzz-artifacts upload)"
    exit 1
fi
