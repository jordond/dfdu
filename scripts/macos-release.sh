#!/bin/sh
# Builds the macOS release files of the GUI: scripts/macos-release.sh OUT_DIR IDENTITY, with
# IDENTITY a Developer ID Application identity in the keychain. Writes, for the target
# (DFDU_TARGET, or the host):
#   dfdu-gui-TARGET.dmg          signed, notarised and stapled, with dfdu.app and /Applications;
#   dfdu-gui-TARGET.app.tar.gz   the same dfdu.app, stapled, for install.sh --gui.
# Notarisation takes the credentials of scripts/macos-notarize.sh; DFDU_NO_NOTARIZE=1 skips it
# (and the Gatekeeper checks, which need it) for a local try of the rest.
set -eu
out=${1:?usage: scripts/macos-release.sh OUT_DIR IDENTITY}
identity=${2:?usage: scripts/macos-release.sh OUT_DIR IDENTITY}
root=$(cd "$(dirname "$0")/.." && pwd)
triple=${DFDU_TARGET:-$(rustc -vV | sed -n 's/^host: //p')}
mkdir -p "$out"
out=$(cd "$out" && pwd)
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT

DFDU_TARGET=$triple "$root/scripts/macos-app.sh" "$work" "$identity" >/dev/null
app="$work/dfdu.app"

# The disk image: the app and a link to /Applications to drag it onto.
mkdir "$work/image"
ditto "$app" "$work/image/dfdu.app"
ln -s /Applications "$work/image/Applications"
dmg="$out/dfdu-gui-$triple.dmg"
rm -f "$dmg"
hdiutil create -quiet -volname dfdu -srcfolder "$work/image" -fs HFS+ -format UDZO "$dmg"
codesign --force --timestamp --sign "$identity" "$dmg"
codesign --verify --strict --verbose=2 "$dmg"

if [ "${DFDU_NO_NOTARIZE:-}" != 1 ]; then
	# Notarising the image also notarises the app inside, so both take a stapled ticket.
	"$root/scripts/macos-notarize.sh" "$dmg"
	xcrun stapler staple "$app"
	spctl --assess --type execute --verbose=2 "$app"
	spctl --assess --type open --context context:primary-signature --verbose=2 "$dmg"
fi

tar -C "$work" -czf "$out/dfdu-gui-$triple.app.tar.gz" dfdu.app
echo "$dmg"
echo "$out/dfdu-gui-$triple.app.tar.gz"
