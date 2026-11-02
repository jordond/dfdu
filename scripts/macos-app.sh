#!/bin/sh
# Builds dfdu.app from a release build of dfdu-gui, with the helper's launch daemon and the icon,
# and signs it with hardened runtime: scripts/macos-app.sh OUT_DIR [IDENTITY]. IDENTITY is a
# codesign identity (a Developer ID Application for releases, Apple Development for local tests
# of the helper); without one the app is signed ad hoc, which runs but cannot use the helper.
# DFDU_TARGET=x86_64-apple-darwin (or aarch64-apple-darwin) builds for that target instead of
# the host. The app needs no entitlements: the hardened runtime's defaults cover wgpu on Metal.
set -eu
out=${1:?usage: scripts/macos-app.sh OUT_DIR [IDENTITY]}
identity=${2:--}
root=$(cd "$(dirname "$0")/.." && pwd)
version=$(cargo metadata --no-deps --format-version 1 --manifest-path "$root/Cargo.toml" |
	sed -n 's/.*"name":"dfdu-gui","version":"\([^"]*\)".*/\1/p')
[ -n "$version" ] || { echo "macos-app.sh: no dfdu-gui version" >&2; exit 1; }
target=${CARGO_TARGET_DIR:-$root/target}
if [ -n "${DFDU_TARGET:-}" ]; then
	cargo build --release --locked -p dfdu-gui --target "$DFDU_TARGET" --manifest-path "$root/Cargo.toml"
	bin="$target/$DFDU_TARGET/release/dfdu-gui"
else
	cargo build --release --locked -p dfdu-gui --manifest-path "$root/Cargo.toml"
	bin="$target/release/dfdu-gui"
fi
mkdir -p "$out"
app="$out/dfdu.app"
rm -rf "$app"
mkdir -p "$app/Contents/MacOS" "$app/Contents/Resources" "$app/Contents/Library/LaunchDaemons"
cp "$bin" "$app/Contents/MacOS/dfdu-gui"
sed "s/@VERSION@/$version/g" "$root/crates/dfdu-gui/macos/Info.plist" >"$app/Contents/Info.plist"
cp "$root/crates/dfdu-gui/macos/app.dfdu.helper.plist" "$app/Contents/Library/LaunchDaemons/"

# The icon, from the 512 px mark: iconutil wants every size up to 512 (and 256@2x).
iconset=$(mktemp -d)/dfdu.iconset
mkdir -p "$iconset"
mark="$root/assets/logo/dfdu-mark-512.png"
for size in 16 32 128 256 512; do
	sips -z "$size" "$size" "$mark" --out "$iconset/icon_${size}x${size}.png" >/dev/null
	double=$((size * 2))
	[ "$double" -le 512 ] &&
		sips -z "$double" "$double" "$mark" --out "$iconset/icon_${size}x${size}@2x.png" >/dev/null
done
iconutil -c icns "$iconset" -o "$app/Contents/Resources/dfdu.icns"
rm -rf "$(dirname "$iconset")"

# A secure timestamp needs a real identity (and the network); ad hoc signatures have none.
stamp=--timestamp
[ "$identity" = - ] && stamp=--timestamp=none
codesign --force --options runtime "$stamp" --sign "$identity" "$app"
codesign --verify --strict --verbose=2 "$app"
echo "$app"
