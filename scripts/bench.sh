#!/usr/bin/env bash
# Times dut against the other disk usage tools found on PATH, on a dut-bench tree (ROADMAP M2).
# Wall time and peak memory come from hyperfine (2.0 or later reports memory).
#
# Usage: scripts/bench.sh [options] WORKDIR
#   --entries N   entries in the tree (default 1000000)
#   --wide N      of which N files in one directory (default 0)
#   --seed N      tree seed (default 1)
#   --runs N      hyperfine runs per tool (default 10)
#   --cold        drop the page cache before every run (needs root or sudo; Linux only)
#
# The tree is generated in WORKDIR once and reused. Results go to WORKDIR/results-*.md.
set -euo pipefail

entries=1000000 wide=0 seed=1 runs=10 cold=0
while [[ $# -gt 1 ]]; do
    case $1 in
        --entries) entries=$2; shift 2 ;;
        --wide) wide=$2; shift 2 ;;
        --seed) seed=$2; shift 2 ;;
        --runs) runs=$2; shift 2 ;;
        --cold) cold=1; shift ;;
        *) echo "unknown option $1" >&2; exit 2 ;;
    esac
done
work=${1:?usage: bench.sh [options] WORKDIR}
here=$(cd "$(dirname "$0")/.." && pwd)

command -v hyperfine >/dev/null || { echo "hyperfine not found" >&2; exit 1; }
cargo build --release --manifest-path "$here/Cargo.toml" -p dut -p dut-bench
dut=$here/target/release/dut

tree=$work/tree-$entries-w$wide-s$seed
if [[ ! -d $tree ]]; then
    mkdir -p "$work"
    "$here/target/release/dut-bench" gen "$tree" --entries "$entries" --wide "$wide" --seed "$seed"
fi

# Each tool with the flags that make it scan and print a summary without a UI.
cmds=("$dut scan --bytes $tree")
names=("dut")
add() {
    if command -v "$1" >/dev/null; then
        names+=("$1")
        cmds+=("$*")
    fi
}
add dua aggregate "$tree"
add gdu -n -p -s "$tree"
add pdu --max-depth 1 "$tree"
add dust -d 0 "$tree"
add ncdu -0 -o /dev/null "$tree"
add du -s "$tree"

args=(-N --runs "$runs" --style basic)
if [[ $cold == 1 ]]; then
    drop='sync; echo 3 > /proc/sys/vm/drop_caches'
    [[ $(id -u) == 0 ]] || drop="sudo sh -c '$drop'"
    args+=(--prepare "$drop")
    mode=cold
else
    args+=(--warmup 2)
    mode=warm
fi
for i in "${!names[@]}"; do
    args+=(--command-name "${names[$i]}" "${cmds[$i]}")
done
out=$work/results-$entries-w$wide-$mode.md
hyperfine "${args[@]}" --export-markdown "$out"
{
    echo
    echo "$(nproc) CPUs ($(grep -m1 'model name' /proc/cpuinfo 2>/dev/null | cut -d: -f2- | xargs)), $mode cache, tree: $entries entries, $wide in one directory, seed $seed."
    for n in "${names[@]}"; do
        case $n in
            dut) v="dut $(git -C "$here" rev-parse --short HEAD)" ;;
            du) v=$(du --version | head -1) ;;
            ncdu) v=$(ncdu -v) ;;
            *) v=$("$n" --version 2>&1 | head -1) ;;
        esac
        echo "- $v"
    done
} >>"$out"
cat "$out"
