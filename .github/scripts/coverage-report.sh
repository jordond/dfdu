#!/usr/bin/env bash
# shellcheck disable=SC2016 # backticks in single quotes are Markdown fences
# Writes lcov.info and the coverage summary from a finished `cargo llvm-cov --no-report` run.
# With a base revision, also writes coverage.md, the PR comment section: workspace and
# changed-line coverage, listing only the changed files with lines left uncovered.
# Usage: coverage-report.sh <min-line-percent> [base-rev]
set -euo pipefail

min=$1
base=${2:-}
summary=${GITHUB_STEP_SUMMARY:-/dev/stdout}

cargo llvm-cov report --lcov --output-path lcov.info
table=$(cargo llvm-cov report --summary-only)
printf '```\n%s\n```\n' "$table" >>"$summary"

[[ -n $base ]] || exit 0

pipx run diff-cover==11.0.0 lcov.info --compare-branch "$base" --format markdown:diff-cover.md
cat diff-cover.md >>"$summary"
lines=$(awk '/^TOTAL/ {print $10}' <<<"$table")
{
    echo '## Coverage'
    echo
    echo "Lines: $lines of the workspace ($min% required)."
    echo
    # Fully covered files, the source excerpts and the per-file table stay in the job summary:
    # on a large pull request they push the comment past GitHub's size limit.
    sed '/^## Diff:/d; /^- .* (100%)$/d; s/^# Diff Coverage/### Changed lines/; s/^## /#### /' diff-cover.md |
        awk '/^#### / && !/^#### Summary/ {exit} {print}'
    echo
    echo 'The job summary has the rest: fully covered files, the uncovered lines and the per-file table.'
} >coverage.md
