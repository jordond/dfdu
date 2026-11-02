#!/usr/bin/env bash
# Fills the package manifests in pkg/ (Scoop, winget, AUR) for a published release, with the
# checksums from the release's sha256.sum, and the GUI's (Scoop, winget) from gui-sha256.sum
# when the release has it. Submitting them is done by hand.
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

# The GUI's checksums (from 1.0 on): gui-sha256.sum, names dfdu-gui-NAME.
files=(scoop/dfdu.json aur/PKGBUILD winget/jordond.dfdu.yaml winget/jordond.dfdu.installer.yaml
    winget/jordond.dfdu.locale.en-US.yaml)
if gui=$(gh release download "v$version" --repo jordond/dfdu --pattern gui-sha256.sum --output - 2>/dev/null); then
    while read -r hash name; do
        name=${name#\*}
        args+=(-e "s/@SHA256_GUI_${name#dfdu-gui-}@/$hash/g")
    done <<<"$gui"
    files+=(scoop/dfdu-gui.json winget/jordond.dfdu-gui.yaml winget/jordond.dfdu-gui.installer.yaml
        winget/jordond.dfdu-gui.locale.en-US.yaml)
else
    echo "v$version has no GUI checksums: CLI manifests only" >&2
fi

for f in "${files[@]}"; do
    mkdir -p "$out/$(dirname "$f")"
    sed "${args[@]}" "$root/pkg/$f" >"$out/$f"
    if grep -q '@[A-Z0-9_a-z.-]*@' "$out/$f"; then
        echo "$out/$f: unfilled placeholder (is the release complete?)" >&2
        exit 1
    fi
done
echo "manifests in $out"
