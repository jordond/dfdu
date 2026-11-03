#!/usr/bin/env bash
# Tests install.sh --gui offline: serves a fake release over http on 127.0.0.1 and points
# install.sh at it with DFDU_RELEASE_BASE. The macOS path runs on macOS only; the Linux
# paths run on any host, with a uname stand-in on PATH that reports Linux.
# Needs python3, and tar with xz support. Usage: scripts/test-install-gui.sh
set -euo pipefail

root="$(cd "$(dirname "$0")/.." && pwd)"
installer="$root/install.sh"
shell="$(command -v dash || echo sh)"
work="$(mktemp -d)"
server=""
cleanup() {
    if [ -n "$server" ]; then kill "$server" 2>/dev/null || true; fi
    rm -rf "$work"
}
trap cleanup EXIT

passed=0
fail() { echo "FAIL: $*" >&2; exit 1; }
pass() { echo "ok: $*"; passed=$((passed + 1)); }

checksum() { # dir name
    if command -v sha256sum >/dev/null; then
        (cd "$1" && sha256sum "$2") >"$1/$2.sha256"
    else
        (cd "$1" && shasum -a 256 "$2") >"$1/$2.sha256"
    fi
}

# A release dir with the CLI archive for each target, and the GUI assets when gui=1.
release() { # version gui
    dir="$work/srv/download/v$1"
    mkdir -p "$dir"
    for target in aarch64-apple-darwin x86_64-apple-darwin x86_64-unknown-linux-musl aarch64-unknown-linux-musl; do
        stage="$work/stage/$1/dfdu-$target"
        mkdir -p "$stage/man" "$stage/completions"
        printf '#!/bin/sh\necho "dfdu %s"\n' "$1" >"$stage/dfdu"
        chmod 755 "$stage/dfdu"
        echo ".TH DFDU 1" >"$stage/man/dfdu.1"
        echo "# bash" >"$stage/completions/dfdu.bash"
        tar -cJf "$dir/dfdu-$target.tar.xz" -C "$work/stage/$1" "dfdu-$target"
        checksum "$dir" "dfdu-$target.tar.xz"
    done
    [ "$2" = 1 ] || return 0
    for target in aarch64-apple-darwin x86_64-apple-darwin; do
        app="$work/stage/$1/app-$target"
        mkdir -p "$app/dfdu.app/Contents/MacOS"
        printf '#!/bin/sh\necho "dfdu gui %s"\n' "$1" >"$app/dfdu.app/Contents/MacOS/dfdu"
        chmod 755 "$app/dfdu.app/Contents/MacOS/dfdu"
        tar -czf "$dir/dfdu-gui-$target.app.tar.gz" -C "$app" dfdu.app
        checksum "$dir" "dfdu-gui-$target.app.tar.gz"
    done
    printf '#!/bin/sh\necho "dfdu gui %s"\n' "$1" >"$dir/dfdu-gui-x86_64-unknown-linux-gnu.AppImage"
    checksum "$dir" "dfdu-gui-x86_64-unknown-linux-gnu.AppImage"
    printf 'PNG' >"$dir/dfdu-gui-icon-512.png"
    checksum "$dir" "dfdu-gui-icon-512.png"
}

release 1.0.0 1
release 1.1.0 1
release 0.9.0 0
release 1.0.1 1
# A GUI asset that does not match its checksum.
echo tampered >>"$work/srv/download/v1.0.1/dfdu-gui-x86_64-unknown-linux-gnu.AppImage"
mkdir -p "$work/srv/latest"
ln -s ../download/v0.9.0 "$work/srv/latest/download"

# uname stand-ins that report Linux on x86_64 and on aarch64.
for arch in x86_64 aarch64; do
    mkdir -p "$work/linux-$arch"
    # shellcheck disable=SC2016 # the $1 and $@ are the stand-in's own.
    printf '#!/bin/sh\ncase "$1" in -s) echo Linux ;; -m) echo %s ;; *) exec /usr/bin/uname "$@" ;; esac\n' "$arch" \
        >"$work/linux-$arch/uname"
    chmod 755 "$work/linux-$arch/uname"
done

port="$(python3 -I -c 'import socket; s = socket.socket(); s.bind(("127.0.0.1", 0)); print(s.getsockname()[1])')"
python3 -I -m http.server --bind 127.0.0.1 --directory "$work/srv" "$port" >"$work/server.log" 2>&1 &
server=$!
for _ in $(seq 50); do
    curl -fsS -o /dev/null "http://127.0.0.1:$port/download/v1.0.0/dfdu-gui-icon-512.png" 2>/dev/null && break
    sleep 0.1
done

# Runs install.sh in a fresh HOME, with XDG_DATA_HOME=$xdg; the exit status is in $rc, stderr in $work/err.
run() { # name [PATH prefix] -- args...
    home="$work/home-$1"
    mkdir -p "$home"
    prefix="$2"
    shift 3
    rc=0
    env HOME="$home" PATH="$prefix${prefix:+:}$PATH" NO_COLOR=1 XDG_DATA_HOME="${xdg:-}" XDG_CONFIG_HOME= \
        DFDU_RELEASE_BASE="http://127.0.0.1:$port" "$shell" "$installer" "$@" 2>"$work/err" || rc=$?
}
expect_err() { grep -qF -- "$1" "$work/err" || { cat "$work/err" >&2; fail "stderr lacks: $1"; }; }

if [ "$(uname -s)" = Darwin ]; then
    run mac "" -- --gui --version 1.0.0
    [ "$rc" = 0 ] || { cat "$work/err" >&2; fail "macOS --gui exited $rc"; }
    [ -x "$home/.local/bin/dfdu" ] || fail "macOS: no dfdu"
    [ "$("$home/Applications/dfdu.app/Contents/MacOS/dfdu")" = "dfdu gui 1.0.0" ] || fail "macOS: no dfdu.app"
    expect_err "open View > Full disk access in dfdu"
    pass "macOS: dfdu.app in ~/Applications"

    run mac "" -- --gui --version 1.1.0
    [ "$("$home/Applications/dfdu.app/Contents/MacOS/dfdu")" = "dfdu gui 1.1.0" ] || fail "macOS: dfdu.app not replaced"
    [ -z "$(find "$home/Applications" -name '.dfdu.app.*')" ] || fail "macOS: staging left behind"
    pass "macOS: a second run replaces dfdu.app"

    (cd "$work" && run mac-dir "" -- --gui --version 1.0.0 --app-dir apps/)
    [ -d "$work/apps/dfdu.app" ] || fail "macOS: --app-dir with a relative path"
    DFDU_APP_DIR="$work/apps-env" DFDU_GUI=1 run mac-env "" -- --version 1.0.0
    [ -d "$work/apps-env/dfdu.app" ] || fail "macOS: DFDU_GUI and DFDU_APP_DIR"
    pass "macOS: --app-dir, DFDU_GUI and DFDU_APP_DIR"
else
    echo "skip: macOS path (not on macOS)"
fi

run linux "$work/linux-x86_64" -- --gui --version 1.0.0
[ "$rc" = 0 ] || { cat "$work/err" >&2; fail "Linux --gui exited $rc"; }
gui="$home/.local/bin/dfdu-gui"
[ -x "$gui" ] || fail "Linux: no executable dfdu-gui"
[ "$(stat -f %Lp "$gui" 2>/dev/null || stat -c %a "$gui")" = 755 ] || fail "Linux: dfdu-gui is not mode 755"
desktop="$home/.local/share/applications/app.dfdu.desktop"
grep -qxF "Exec=\"$gui\"" "$desktop" || fail "Linux: desktop entry has no Exec for $gui"
grep -qxF "Categories=System;Utility;FileTools;" "$desktop" || fail "Linux: desktop entry categories"
[ -f "$home/.local/share/icons/hicolor/512x512/apps/app.dfdu.png" ] || fail "Linux: no icon"
pass "Linux: dfdu-gui, desktop entry and icon"
if grep -qF "AppImages need FUSE 2" "$work/err"; then echo "    (FUSE 2 warning shown)"; fi

xdg="$work/xdg" run linux-xdg "$work/linux-x86_64" -- --gui --version 1.0.0
[ -f "$work/xdg/applications/app.dfdu.desktop" ] || fail "Linux: XDG_DATA_HOME not respected"
[ -f "$work/xdg/icons/hicolor/512x512/apps/app.dfdu.png" ] || fail "Linux: icon not under XDG_DATA_HOME"
pass "Linux: XDG_DATA_HOME"

run linux-bare "$work/linux-x86_64" -- --gui --no-extras --version 1.0.0
[ "$rc" = 0 ] && [ -x "$home/.local/bin/dfdu-gui" ] || fail "Linux --no-extras: no dfdu-gui"
[ ! -e "$home/.local/share/applications" ] || fail "Linux --no-extras: desktop entry written"
pass "Linux: --no-extras skips the desktop entry"

run old "$work/linux-x86_64" -- --gui --version 0.9.0
[ "$rc" != 0 ] || fail "--gui on 0.9.0 succeeded"
[ -x "$home/.local/bin/dfdu" ] || fail "--gui on 0.9.0: the CLI is not installed"
expect_err "dfdu 0.9.0 has no GUI build; the GUI ships from 0.3.0"
pass "a release with no GUI fails after the CLI is installed"

run latest "$work/linux-x86_64" -- --gui
[ "$rc" != 0 ] || fail "--gui on the latest (0.9.0) succeeded"
expect_err "dfdu 0.9.0 has no GUI build; the GUI ships from 0.3.0"
pass "the latest release names its version"

run arm "$work/linux-aarch64" -- --gui --version 1.0.0
[ "$rc" != 0 ] && [ -x "$home/.local/bin/dfdu" ] || fail "aarch64 Linux: wrong outcome"
expect_err "no build for aarch64 Linux"
pass "aarch64 Linux: CLI installed, GUI refused"

run tampered "$work/linux-x86_64" -- --gui --version 1.0.1
[ "$rc" != 0 ] && [ ! -e "$home/.local/bin/dfdu-gui" ] || fail "a tampered AppImage was installed"
expect_err "checksum mismatch for dfdu-gui-x86_64-unknown-linux-gnu.AppImage"
pass "a GUI asset that fails its checksum is not installed"

echo "$passed passed"
