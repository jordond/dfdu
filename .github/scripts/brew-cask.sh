#!/usr/bin/env bash
# Fills pkg/homebrew/dfdu-gui.rb for a release from the GUI's checksums:
# brew-cask.sh VERSION SUMS OUT, with SUMS lines "HASH  dfdu-gui-NAME" (gui-sha256.sum).
set -euo pipefail
version=${1:?usage: brew-cask.sh VERSION SUMS OUT}
sums=${2:?usage: brew-cask.sh VERSION SUMS OUT}
out=${3:?usage: brew-cask.sh VERSION SUMS OUT}
root=$(cd "$(dirname "$0")/../.." && pwd)
args=(-e "s/@VERSION@/$version/g")
while read -r hash name; do
    name=${name#\*}
    args+=(-e "s/@SHA256_${name#dfdu-gui-}@/$hash/g")
done <"$sums"
sed "${args[@]}" "$root/pkg/homebrew/dfdu-gui.rb" >"$out"
if grep -q '@[A-Za-z0-9_.-]*@' "$out"; then
    echo "::error::$out: unfilled placeholder (are both macOS images in $sums?)" >&2
    exit 1
fi
