#!/bin/sh
# Builds dfdu.app from a release build of dfdu-gui, with the helper's launch daemon, and signs
# it with hardened runtime: scripts/macos-app.sh OUT_DIR [IDENTITY]. IDENTITY is a codesign
# identity (a Developer ID Application for releases, Apple Development for local tests of the
# helper); without one the app is signed ad hoc, which runs but cannot use the helper.
set -eu
out=${1:?usage: scripts/macos-app.sh OUT_DIR [IDENTITY]}
identity=${2:--}
root=$(cd "$(dirname "$0")/.." && pwd)
version=$(cargo metadata --no-deps --format-version 1 --manifest-path "$root/Cargo.toml" |
	sed -n 's/.*"name":"dfdu-gui","version":"\([^"]*\)".*/\1/p')
[ -n "$version" ] || { echo "macos-app.sh: no dfdu-gui version" >&2; exit 1; }
cargo build --release --locked -p dfdu-gui --manifest-path "$root/Cargo.toml"
target=${CARGO_TARGET_DIR:-$root/target}
app="$out/dfdu.app"
rm -rf "$app"
mkdir -p "$app/Contents/MacOS" "$app/Contents/Library/LaunchDaemons"
cp "$target/release/dfdu-gui" "$app/Contents/MacOS/dfdu-gui"
sed "s/@VERSION@/$version/g" "$root/crates/dfdu-gui/macos/Info.plist" >"$app/Contents/Info.plist"
cp "$root/crates/dfdu-gui/macos/app.dfdu.helper.plist" "$app/Contents/Library/LaunchDaemons/"
# A secure timestamp needs a real identity (and the network); ad hoc signatures have none.
stamp=--timestamp
[ "$identity" = - ] && stamp=--timestamp=none
codesign --force --options runtime "$stamp" --sign "$identity" "$app"
codesign --verify --strict --verbose=2 "$app"
echo "$app"
