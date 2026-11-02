#!/bin/sh
# Writes the App Store Connect API key in NOTARY_KEY_P8 to FILE for notarytool:
# .github/scripts/notary-key.sh FILE. The secret may hold the .p8 as is or base64-encoded.
set -eu
file=${1:?usage: notary-key.sh FILE}
: "${NOTARY_KEY_P8:?notary-key.sh: NOTARY_KEY_P8 is empty}"
umask 077
case $NOTARY_KEY_P8 in
-----BEGIN*) printf '%s\n' "$NOTARY_KEY_P8" >"$file" ;;
*) printf '%s' "$NOTARY_KEY_P8" | tr -d '\r\n ' | base64 -D >"$file" ;;
esac
grep -q -- '-----BEGIN PRIVATE KEY-----' "$file" || { echo "notary-key.sh: not a .p8 key" >&2; exit 1; }
