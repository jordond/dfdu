#!/usr/bin/env bash
# Builds the tree the README recordings browse: a few projects with dependencies and build
# output, caches and downloads, with sizes in real blocks and old enough times to be recommended.
#
# Usage: scripts/vhs/demo-tree.sh DIR
set -euo pipefail
dir=${1:?usage: $0 DIR}
mkdir -p "$dir"
cd "$dir"

blob() { # PATH MiB
    mkdir -p "$(dirname "$1")"
    head -c $(($2 * 1048576)) /dev/urandom >"$1"
}

for p in web-shop admin-panel landing; do
    mkdir -p "dev/$p/src"
    echo '{}' >"dev/$p/package.json"
    echo '{"lockfileVersion": 3}' >"dev/$p/package-lock.json"
    echo 'export {}' >"dev/$p/src/index.ts"
    for m in react typescript esbuild webpack; do
        blob "dev/$p/node_modules/$m/dist/index.js" $((RANDOM % 18 + 4))
    done
    blob "dev/$p/dist/bundle.js" 3
done
for p in indexer cli-tool; do
    mkdir -p "dev/$p/src"
    printf '[package]\nname = "%s"\n' "$p" >"dev/$p/Cargo.toml"
    echo 'fn main() {}' >"dev/$p/src/main.rs"
    blob "dev/$p/target/debug/deps/lib$p.rlib" $((RANDOM % 40 + 30))
    blob "dev/$p/target/release/$p" 12
done
mkdir -p dev/notebook
echo 'numpy' >dev/notebook/requirements.txt
blob dev/notebook/.venv/lib/python3.13/site-packages/numpy/core.so 28
echo 'home = /usr/bin' >dev/notebook/.venv/pyvenv.cfg
blob Downloads/ubuntu-24.04-desktop.iso 120
blob Downloads/screen-recording.mov 45
blob Pictures/2026/trip/IMG_0412.heic 6
blob Pictures/2026/trip/IMG_0413.heic 5
blob Documents/taxes-2025.pdf 2
blob .cache/pip/http/wheels.bin 24
# Old enough for the rules' age thresholds.
find . -exec touch -t 202606010000 {} +
