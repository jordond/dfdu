#!/bin/sh
# Notarises FILE (.dmg, .zip or .pkg) and staples the ticket to a .dmg or .pkg:
# scripts/macos-notarize.sh FILE. Credentials, one of:
#   NOTARY_PROFILE  a keychain profile saved with `xcrun notarytool store-credentials`;
#   NOTARY_KEY, NOTARY_KEY_ID, NOTARY_ISSUER  an App Store Connect API key (path to the .p8,
#                   its key id, and the issuer id from App Store Connect > Users and Access).
# Prints Apple's log and fails when the submission is not accepted.
set -eu
file=${1:?usage: scripts/macos-notarize.sh FILE}
if [ -n "${NOTARY_PROFILE:-}" ]; then
	set -- --keychain-profile "$NOTARY_PROFILE"
else
	: "${NOTARY_KEY:?macos-notarize.sh: set NOTARY_PROFILE, or NOTARY_KEY, NOTARY_KEY_ID and NOTARY_ISSUER}"
	set -- --key "$NOTARY_KEY" --key-id "${NOTARY_KEY_ID:?}" --issuer "${NOTARY_ISSUER:?}"
fi
result=$(mktemp)
trap 'rm -f "$result"' EXIT
# notarytool exits 0 for a rejected submission too, so the status decides.
xcrun notarytool submit "$file" "$@" --wait --timeout 30m --output-format plist >"$result"
status=$(plutil -extract status raw -o - "$result")
id=$(plutil -extract id raw -o - "$result")
echo "macos-notarize.sh: $file: $status ($id)"
if [ "$status" != Accepted ]; then
	xcrun notarytool log "$id" "$@" >&2 || true
	exit 1
fi
case $file in
*.dmg | *.pkg)
	xcrun stapler staple "$file"
	xcrun stapler validate "$file"
	;;
esac
