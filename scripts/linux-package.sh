#!/bin/sh
# Builds the Linux release files of the GUI for x86_64 glibc: scripts/linux-package.sh OUT_DIR.
# Writes:
#   dfdu-gui-x86_64-unknown-linux-gnu.deb       /usr/bin/dfdu-gui, the agent script at
#     /usr/libexec/dfdu/dfdu-agent and its polkit action app.dfdu.agent, so an administrator's
#     scan asks for the password in dfdu's name;
#   dfdu-gui-x86_64-unknown-linux-gnu.AppImage  the same program in one file; its agent is the
#     AppImage itself, through pkexec's own action.
# Both carry the desktop file app.dfdu.desktop and the icon. Sources are in crates/dfdu-gui/linux.
#
# The .deb is put together with dpkg-deb, not cargo-packager: cargo-packager 0.11.8 names the
# desktop file after the binary, cannot write Recommends, and its AppImage takes linuxdeploy
# and an unpinned plugin from the network. The AppImage is made with appimagetool and the type 2
# runtime, both pinned below by version and SHA-256. Needs on the build machine: cargo, dpkg-deb,
# objdump (binutils), curl, sha256sum, and desktop-file-validate (desktop-file-utils).
set -eu
out=${1:?usage: scripts/linux-package.sh OUT_DIR}
root=$(cd "$(dirname "$0")/.." && pwd)
triple=x86_64-unknown-linux-gnu
name=dfdu-gui-$triple
linux=$root/crates/dfdu-gui/linux

appimagetool_url=https://github.com/AppImage/appimagetool/releases/download/1.9.1/appimagetool-x86_64.AppImage
appimagetool_sha256=ed4ce84f0d9caff66f50bcca6ff6f35aae54ce8135408b3fa33abfc3cb384eb0
runtime_url=https://github.com/AppImage/type2-runtime/releases/download/20251108/runtime-x86_64
runtime_sha256=2fca8b443c92510f1483a883f60061ad09b46b978b2631c807cd873a47ec260d

die() {
	echo "linux-package.sh: $*" >&2
	exit 1
}

for tool in cargo dpkg-deb objdump curl sha256sum desktop-file-validate; do
	command -v "$tool" >/dev/null || die "$tool is not installed"
done

version=$(sed -n 's/^version = "\(.*\)"$/\1/p' "$root/crates/dfdu-gui/Cargo.toml" | head -n 1)
[ -n "$version" ] || die "no version in crates/dfdu-gui/Cargo.toml"
target_dir=${CARGO_TARGET_DIR:-$root/target}
case $target_dir in
/*) ;;
*) target_dir=$PWD/$target_dir ;;
esac
mkdir -p "$out"
out=$(cd "$out" && pwd)
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT

cargo build --release --locked -p dfdu-gui --target "$triple" --manifest-path "$root/Cargo.toml"
bin=$target_dir/$triple/release/dfdu-gui
[ -x "$bin" ] || die "no binary at $bin"

# The libraries linked at build time must be glibc's and libgcc's alone, which Depends names:
# the window and GL libraries are loaded at run time, and SQLite is built in.
needed=$(objdump -p "$bin" | sed -n 's/^ *NEEDED *//p')
for lib in $needed; do
	case $lib in
	libc.so.6 | libm.so.6 | libgcc_s.so.1 | libpthread.so.0 | libdl.so.2 | librt.so.1 | libutil.so.1 | ld-linux-x86-64.so.2) ;;
	*) die "dfdu-gui links $lib, which the .deb does not depend on" ;;
	esac
done
glibc=$(objdump -T "$bin" | sed -n 's/.*GLIBC_\([0-9][0-9.]*\).*/\1/p' | sort -u -t. -k1,1n -k2,2n -k3,3n | tail -n 1)
[ -n "$glibc" ] || die "cannot tell which glibc dfdu-gui needs"

desktop-file-validate "$linux/app.dfdu.desktop"

# What both packages carry, under the prefix /usr of $1.
stage() {
	install -D -m 0755 "$bin" "$1/usr/bin/dfdu-gui"
	install -D -m 0644 "$linux/app.dfdu.desktop" "$1/usr/share/applications/app.dfdu.desktop"
	install -D -m 0644 "$root/assets/logo/dfdu-mark-512.png" \
		"$1/usr/share/icons/hicolor/512x512/apps/app.dfdu.png"
}

# The .deb.
deb=$work/deb
stage "$deb"
install -D -m 0755 "$linux/dfdu-agent" "$deb/usr/libexec/dfdu/dfdu-agent"
install -D -m 0644 "$linux/app.dfdu.agent.policy" "$deb/usr/share/polkit-1/actions/app.dfdu.agent.policy"
install -D -m 0644 "$root/LICENSE" "$deb/usr/share/doc/dfdu-gui/copyright"
# The libraries eframe, winit and wgpu load at run time: Wayland and X11 windows, the keyboard,
# and EGL or GLX for the OpenGL renderer; Vulkan only for wgpu's first choice, which falls back.
depends="libc6 (>= $glibc), libgcc-s1, libwayland-client0, libwayland-cursor0, libwayland-egl1, \
libx11-6, libx11-xcb1, libxcb1, libxcursor1, libxrandr2, libxi6, libxkbcommon0, \
libxkbcommon-x11-0, libegl1, libgl1"
mkdir -p "$deb/DEBIAN"
cat >"$deb/DEBIAN/control" <<EOF
Package: dfdu-gui
Version: $version
Architecture: amd64
Maintainer: Jordon de Hoog <jordon.dehoog@gmail.com>
Installed-Size: $(du -sk "$deb/usr" | cut -f 1)
Depends: $depends
Recommends: pkexec | policykit-1, libvulkan1
Section: utils
Priority: optional
Homepage: https://github.com/jordond/dfdu
Description: Desktop browser for dfdu, in a window
 See what takes up disk space and clean it up: a list, a map, cleanup
 candidates, duplicates and the largest files, with the scans, marks,
 cleanup rules, delete gates and undo of the dfdu terminal browser.
 Scanning as an administrator goes through polkit (pkexec).
EOF
dpkg-deb --root-owner-group -Zxz --build "$deb" "$out/$name.deb" >/dev/null

# The AppImage: the AppDir has the desktop file and icon at its top, and AppRun to start.
appdir=$work/AppDir
stage "$appdir"
install -m 0755 "$linux/AppRun" "$appdir/AppRun"
cp "$linux/app.dfdu.desktop" "$appdir/app.dfdu.desktop"
cp "$root/assets/logo/dfdu-mark-512.png" "$appdir/app.dfdu.png"
ln -s app.dfdu.png "$appdir/.DirIcon"

curl -fsSL --retry 3 -o "$work/appimagetool" "$appimagetool_url"
curl -fsSL --retry 3 -o "$work/runtime" "$runtime_url"
printf '%s  %s\n%s  %s\n' "$appimagetool_sha256" "$work/appimagetool" "$runtime_sha256" "$work/runtime" |
	sha256sum -c --quiet -
chmod +x "$work/appimagetool"
# Extracted rather than mounted, so the build needs no FUSE.
rm -f "$out/$name.AppImage"
ARCH=x86_64 APPIMAGE_EXTRACT_AND_RUN=1 \
	SOURCE_DATE_EPOCH=${SOURCE_DATE_EPOCH:-$(git -C "$root" log -1 --format=%ct 2>/dev/null || date +%s)} \
	"$work/appimagetool" --no-appstream --runtime-file "$work/runtime" "$appdir" "$out/$name.AppImage" >&2

echo "$out/$name.deb"
echo "$out/$name.AppImage"
