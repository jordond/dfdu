#!/usr/bin/env bash
# Builds the Windows release files of the GUI: scripts/windows-package.sh OUT_DIR. Writes, for
# the target (DFDU_TARGET, or x86_64-pc-windows-msvc):
#   dfdu-gui-TARGET-setup.exe   an NSIS installer made by cargo-packager from
#                               crates/dfdu-gui/windows/Packager.toml: installs for the current
#                               user (no UAC prompt), with a Start menu shortcut and an uninstaller;
#   dfdu-gui-TARGET.zip         dfdu-gui.exe, LICENSE and README.txt, to run without installing.
# Neither is signed (1.0 ships unsigned). Runs on Windows in Git Bash, as CI's runner does, with
# cargo-packager 0.11.8 installed (cargo install cargo-packager --version 0.11.8 --locked). The
# exe carries dfdu's icon from crates/dfdu-gui/build.rs.
set -euo pipefail
out=${1:?usage: scripts/windows-package.sh OUT_DIR}
root=$(cd "$(dirname "$0")/.." && pwd)
triple=${DFDU_TARGET:-x86_64-pc-windows-msvc}
packager=0.11.8

have=$(cargo packager --version 2>/dev/null || true)
case $have in
*" $packager") ;;
*)
	echo "windows-package.sh: needs cargo-packager $packager (have: ${have:-none});" \
		"cargo install cargo-packager --version $packager --locked" >&2
	exit 1
	;;
esac

version=$(cargo metadata --no-deps --format-version 1 --manifest-path "$root/Cargo.toml" |
	sed -n 's/.*"name":"dfdu-gui","version":"\([^"]*\)".*/\1/p')
[ -n "$version" ] || {
	echo "windows-package.sh: no dfdu-gui version" >&2
	exit 1
}

cargo build --release --locked -p dfdu-gui --target "$triple" --manifest-path "$root/Cargo.toml"
target=${CARGO_TARGET_DIR:-$root/target}
# CARGO_TARGET_DIR may be a Windows path (D:\a\...); the copy below wants a POSIX one.
command -v cygpath >/dev/null && target=$(cygpath -u "$target")
exe="$target/$triple/release/dfdu-gui.exe"
[ -f "$exe" ] || {
	echo "windows-package.sh: no $exe" >&2
	exit 1
}

mkdir -p "$out"
out=$(cd "$out" && pwd)
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT

# The staging directory the packager's config names its files from (see Packager.toml).
stage="$work/stage"
mkdir -p "$stage/out"
cp "$exe" "$stage/dfdu-gui.exe"
cp "$root/crates/dfdu-gui/windows/dfdu.ico" "$stage/"
cp "$root/LICENSE" "$stage/LICENSE"
sed "s/@VERSION@/$version/" "$root/crates/dfdu-gui/windows/Packager.toml" >"$stage/Packager.toml"
cat >"$stage/README.txt" <<EOF
dfdu $version (dfdu-gui) for Windows

dfdu-gui.exe is dfdu's disk usage browser in a window. Run it to see the disks, or give it a
folder: dfdu-gui.exe C:\\Users. dfdu-gui.exe --help lists the options.

It needs no installation; settings and the scan history are kept in dfdu's state directory
(DFDU_STATE_DIR moves it). The installer, dfdu-gui-$triple-setup.exe, puts the same program in
%LOCALAPPDATA%\\dfdu with a Start menu shortcut and an uninstaller.

The terminal version, dfdu, and the source: https://github.com/jordond/dfdu
Licensed under the MIT license (LICENSE).
EOF

# Relative paths only: cargo-packager is a Windows program and reads no MSYS paths.
(cd "$stage" && cargo packager --config Packager.toml --out-dir out --formats nsis --target "$triple")
shopt -s nullglob
made=("$stage"/out/*-setup.exe)
[ "${#made[@]}" -eq 1 ] || {
	echo "windows-package.sh: expected one installer in $stage/out, found ${#made[@]}" >&2
	exit 1
}
setup="$out/dfdu-gui-$triple-setup.exe"
mv "${made[0]}" "$setup"

# The portable zip, with Windows' own bsdtar: Git Bash has no zip, and its tar writes none.
zip="$out/dfdu-gui-$triple.zip"
rm -f "$zip"
bsdtar="$(cygpath -u "${SYSTEMROOT:-C:\\Windows}")/System32/tar.exe"
(cd "$stage" && "$bsdtar" -a -cf portable.zip dfdu-gui.exe LICENSE README.txt)
mv "$stage/portable.zip" "$zip"

echo "$setup"
echo "$zip"
