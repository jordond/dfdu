#!/usr/bin/env bash
# Pushes a PKGBUILD and its .SRCINFO to an AUR package's repository, creating the package on
# its first push. Does nothing when they are what the AUR already has. A -git package's pkgver
# comes from pkgver() at build time, so it is pushed only when the PKGBUILD changes in another
# line, not for each release. Expects GIT_SSH_COMMAND to name a key the AUR accepts.
# Usage: aur-push.sh PKGNAME DIR VERSION   (DIR holds PKGBUILD and .SRCINFO)
set -euo pipefail
pkg=${1:?usage: aur-push.sh PKGNAME DIR VERSION}
dir=${2:?usage: aur-push.sh PKGNAME DIR VERSION}
version=${3:?usage: aur-push.sh PKGNAME DIR VERSION}

repo=$(mktemp -d)
# Cloning a package that doesn't exist yet gives an empty repository.
git -c init.defaultBranch=master clone "ssh://aur@aur.archlinux.org/$pkg.git" "$repo"
if [[ $pkg == *-git && -f $repo/PKGBUILD ]] &&
    diff -q <(grep -v '^pkgver=' "$repo/PKGBUILD") <(grep -v '^pkgver=' "$dir/PKGBUILD") >/dev/null; then
    echo "$pkg: PKGBUILD unchanged apart from pkgver"
    exit 0
fi
cp "$dir/PKGBUILD" "$dir/.SRCINFO" "$repo/"
cd "$repo"
git config user.name "Jordon de Hoog"
git config user.email "jordon.dehoog@gmail.com"
git add PKGBUILD .SRCINFO
if git diff --cached --quiet; then
    echo "$pkg: already at $version"
    exit 0
fi
git commit -m "$pkg $version"
git push origin HEAD:master
