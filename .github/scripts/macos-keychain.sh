#!/bin/sh
# Imports the Developer ID certificate (CERT_P12, a base64 .p12, and CERT_PASSWORD) into a new
# keychain at the front of the search list, so codesign finds it without a prompt:
# .github/scripts/macos-keychain.sh KEYCHAIN. CI only; the keychain lasts as long as the runner.
set -eu
keychain=${1:?usage: macos-keychain.sh KEYCHAIN}
: "${CERT_P12:?macos-keychain.sh: CERT_P12 is empty}"
: "${CERT_PASSWORD:?macos-keychain.sh: CERT_PASSWORD is empty}"
password=$(uuidgen)
p12=$(mktemp)
trap 'rm -f "$p12"' EXIT
printf '%s' "$CERT_P12" | tr -d '\r\n ' | base64 -D >"$p12"
security create-keychain -p "$password" "$keychain"
security set-keychain-settings -lut 21600 "$keychain"
security unlock-keychain -p "$password" "$keychain"
security import "$p12" -k "$keychain" -P "$CERT_PASSWORD" -f pkcs12 -A -T /usr/bin/codesign
security set-key-partition-list -S apple-tool:,apple:,codesign: -s -k "$password" "$keychain" >/dev/null
# shellcheck disable=SC2046 # the search list is one path per line, quoted
security list-keychains -d user -s "$keychain" $(security list-keychains -d user | tr -d '"')
security find-identity -v -p codesigning "$keychain"
