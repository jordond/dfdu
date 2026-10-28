#!/usr/bin/env bash
# Fills the package manifests in pkg/ (Scoop, winget, AUR) for a published release, with the
# checksums from the release's sha256.sum. Submitting them is done by hand.
#
# Usage: scripts/package-manifests.sh VERSION [OUTDIR]   (default OUTDIR: target/pkg/VERSION)
set -euo pipefail

version=${1:?usage: $0 VERSION [OUTDIR]}
out=${2:-target/pkg/$version}
root=$(cd "$(dirname "$0")/.." && pwd)
mkdir -p "$out"
sums=$(gh release download "v$version" --repo jordond/dfdu --pattern sha256.sum --output -)

args=(-e "s/@VERSION@/$version/g")
while read -r hash name; do
    name=${name#\*}
    target=${name#dfdu-}
    target=${target%.tar.xz}
    target=${target%.zip}
    args+=(-e "s/@SHA256_${target}@/$hash/g")
done <<<"$sums"

for f in scoop/dfdu.json aur/PKGBUILD winget/jordond.dfdu.yaml winget/jordond.dfdu.installer.yaml \
    winget/jordond.dfdu.locale.en-US.yaml; do
    mkdir -p "$out/$(dirname "$f")"
    sed "${args[@]}" "$root/pkg/$f" >"$out/$f"
    if grep -q '@[A-Z0-9_a-z-]*@' "$out/$f"; then
        echo "$out/$f: unfilled placeholder (is the release complete?)" >&2
        exit 1
    fi
done
echo "manifests in $out"
