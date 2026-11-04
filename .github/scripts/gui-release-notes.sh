#!/usr/bin/env bash
# Adds the GUI's install and download section to a release's notes, after dist's: run by
# gui-release.yml once the GUI's files are uploaded. Replaces an earlier GUI section, so a rerun
# leaves one.
#
# Usage: gui-release-notes.sh TAG [PRERELEASE]   (PRERELEASE true: no Homebrew cask line)
# Needs gh with GH_TOKEN, and GITHUB_REPOSITORY (owner/name).
set -euo pipefail

tag=$1
prerelease=${2:-false}
repo=${GITHUB_REPOSITORY:?}
version=${tag#v}
base="https://github.com/$repo/releases/download/$tag"
begin='<!-- dfdu-gui:begin -->'
end='<!-- dfdu-gui:end -->'

row() { # FILE PLATFORM
  printf '| [%s](%s/%s) | %s | [checksum](%s/%s.sha256) |\n' "$1" "$base" "$1" "$2" "$base" "$1"
}

section() {
  echo "$begin"
  echo "## dfdu desktop app $version"
  echo
  # One line per paragraph: GitHub shows a release's line breaks as they are.
  echo "The dfdu browser in a window: the same scans, marks, cleanup rules, delete gates and undo" \
    "as the terminal browser, with List, Map, Cleanup, Duplicates and Largest views. The macOS" \
    "app is signed and notarised; the Windows files are not signed yet, so SmartScreen warns on" \
    "the first run."
  echo
  echo "### Install the desktop app"
  echo
  if [ "$prerelease" != true ]; then
    echo "macOS, with Homebrew:"
    echo
    echo '```sh'
    echo "brew install --cask jordond/tap/dfdu-gui"
    echo '```'
    echo
  fi
  echo "macOS and Linux, with the CLI:"
  echo
  echo '```sh'
  echo "curl -fsSL https://dfdu.app/install.sh | sh -s -- --gui --version $version"
  echo '```'
  echo
  echo "Or download a file below: on macOS open the .dmg and drag dfdu to Applications; on Linux" \
    "install the .deb (\`sudo apt install ./dfdu-gui-x86_64-unknown-linux-gnu.deb\`, or the aarch64 one) or run the" \
    "AppImage; on Windows run the installer (per user, no administrator) or unzip the portable zip."
  echo
  echo "### Download the desktop app"
  echo
  echo "|  File  | Platform | Checksum |"
  echo "|--------|----------|----------|"
  row "dfdu-gui-aarch64-apple-darwin.dmg" "Apple Silicon macOS"
  row "dfdu-gui-x86_64-apple-darwin.dmg" "Intel macOS"
  row "dfdu-gui-aarch64-apple-darwin.app.tar.gz" "Apple Silicon macOS (app only)"
  row "dfdu-gui-x86_64-apple-darwin.app.tar.gz" "Intel macOS (app only)"
  row "dfdu-gui-x86_64-unknown-linux-gnu.deb" "x64 Linux (Debian, Ubuntu)"
  row "dfdu-gui-x86_64-unknown-linux-gnu.AppImage" "x64 Linux"
  row "dfdu-gui-aarch64-unknown-linux-gnu.deb" "ARM64 Linux (Debian, Ubuntu)"
  row "dfdu-gui-aarch64-unknown-linux-gnu.AppImage" "ARM64 Linux"
  row "dfdu-gui-x86_64-pc-windows-msvc-setup.exe" "x64 Windows (installer)"
  row "dfdu-gui-x86_64-pc-windows-msvc.zip" "x64 Windows (portable)"
  echo
  echo "All the desktop app's checksums: [gui-sha256.sum]($base/gui-sha256.sum)"
  echo "$end"
}

body=$(gh release view "$tag" --repo "$repo" --json body --jq .body)
# Drop an earlier GUI section (from a rerun), keeping what dist wrote.
body=$(printf '%s\n' "$body" | awk -v b="$begin" -v e="$end" '$0==b{skip=1;next} $0==e{skip=0;next} !skip')
notes=$(mktemp)
trap 'rm -f "$notes"' EXIT
{
  printf '%s\n\n' "$body"
  section
} >"$notes"
gh release edit "$tag" --repo "$repo" --notes-file "$notes"
