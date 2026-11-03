#!/usr/bin/env bash
# The GUI screenshot container's entry point (`cargo xtask demo --gui`): serves the home fixture
# at /home/demo, starts an X server at twice the scale, and takes each named shot of gui/shots
# with dfdu-gui's DFDU_GUI_SCREENSHOT switch, at the fixture's time. Writes out/NAME.png.
#
# Usage: run.sh NAME...
set -euo pipefail
cd /work
fixture=fixtures/home.toml
mkdir -p out /home/demo

ready=$(mktemp -u)
dfdu-demofs mount "$fixture" /home/demo --ready "$ready" &
for _ in $(seq 100); do
    [ -e "$ready" ] && break
    sleep 0.1
done
[ -e "$ready" ] || { echo "run.sh: $fixture did not mount at /home/demo" >&2; exit 1; }

# The window is 1100x720 points, so 2200x1440 pixels at scale 2; the screen leaves room around it.
Xvfb :99 -screen 0 2400x1600x24 -nolisten tcp >/dev/null 2>&1 &
export DISPLAY=:99 WINIT_X11_SCALE_FACTOR=2
for _ in $(seq 100); do
    xdotool getdisplaygeometry >/dev/null 2>&1 && break
    sleep 0.1
done

# As the TUI recordings: libfaketime sets the clock to the fixture's `now` (UTC), and leaves
# monotonic time alone so the window's timers run at their speed.
now=$(dfdu-demofs now "$fixture")

# shot NAME THEME KEYS: takes out/NAME.png.
shot() {
    local name=$1 theme=$2 keys=$3 state mover
    [ "$keys" = - ] && keys=
    keys=${keys//\\s/ }
    keys=${keys//\\n/$'\n'}
    # Settings, history and recent scans of this shot only, out of the scanned home.
    state=$(mktemp -d)
    mkdir -p "$state/dfdu"
    # The pointer, out of the window, so no tile shows its tooltip: moved again while the window
    # opens, as the X server puts it back in the middle.
    (for _ in $(seq 10); do xdotool mousemove 2399 1599; sleep 0.5; done) &
    mover=$!
    env HOME=/home/demo TZ=UTC \
        XDG_CONFIG_HOME="$state/config" XDG_DATA_HOME="$state/data" \
        XDG_STATE_HOME="$state/state" XDG_CACHE_HOME="$state/cache" DFDU_STATE_DIR="$state/dfdu" \
        LD_PRELOAD=/usr/local/lib/libfaketime.so.1 FAKETIME="$now" FAKETIME_DONT_FAKE_MONOTONIC=1 \
        DFDU_GUI_SCREENSHOT="out/$name.ppm" DFDU_GUI_SCREENSHOT_KEYS="$keys" \
        timeout 120 dfdu-gui /home/demo --theme "$theme" >"$state/log" 2>&1 || true
    kill "$mover" 2>/dev/null || true
    wait "$mover" 2>/dev/null || true
    if [ ! -s "out/$name.ppm" ]; then
        echo "run.sh: dfdu-gui wrote no picture for $name" >&2
        cat "$state/log" >&2
        return 1
    fi
    pnmtopng "out/$name.ppm" >"out/$name.png"
    rm "out/$name.ppm"
    echo "took $name"
}

failed=()
for want in "$@"; do
    found=
    while read -r name theme keys; do
        [ "$name" = "$want" ] || continue
        found=1
        shot "$name" "$theme" "$keys" || failed+=("$name")
    done < <(grep -v '^[[:space:]]*\(#\|$\)' gui/shots)
    [ -n "$found" ] || { echo "run.sh: no shot $want in gui/shots" >&2; failed+=("$want"); }
done
if [ ${#failed[@]} -gt 0 ]; then
    echo "run.sh: failed: ${failed[*]}" >&2
    exit 1
fi
