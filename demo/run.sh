#!/usr/bin/env bash
# The recording container's entry point: builds the root the recording shell runs in, then
# records the tape. Lines in the tape set it up:
#
#   # fixture: NAME          the home to serve (fixtures/NAME.toml; default `home`)
#   # mount: NAME PATH       also serve fixtures/NAME.toml at PATH (a disk of its own)
#   # history: NAME AGE      first record a scan of ~ served from fixtures/NAME.toml, AGE
#                            (`7d`) before the fixture's time, for `dfdu diff` and growth notes
#
# The root (/newroot) is fixtures/system.toml with the home fixture at home/demo, all one
# dfdu-demofs mount, so `/` is the fixture's disk. env.sh chroots the shell into it: the mount
# table then shows only what is below it, and the disk list none of the Docker host's disks.
# Kernel filesystems are mounted for /proc and /dev (the disk list leaves them out, and what
# is mounted below them).
#
# Usage: run.sh NAME   (tapes/NAME.tape; writes out/)
set -euo pipefail
name=${1:?usage: run.sh NAME}
cd /work
tape=tapes/$name.tape
root=/newroot

# directive KEY: the values of the tape's `# KEY: ...` lines, one per line.
directive() {
    sed -n "s/^# $1: *//p" "$tape"
}

# mount_fixture FIXTURE DIR [ARGS...]: serves FIXTURE at DIR, in the background, until it
# answers.
mount_fixture() {
    local ready
    ready=$(mktemp -u)
    dfdu-demofs mount "$1" "$2" --ready "$ready" "${@:3}" &
    for _ in $(seq 100); do
        [ -e "$ready" ] && return
        sleep 0.1
    done
    echo "run.sh: $1 did not mount at $2" >&2
    exit 1
}

# build_root FIXTURE: the root at $root with FIXTURE as the demo user's home.
build_root() {
    mkdir -p "$root"
    mount_fixture "$1" "$root" --root fixtures/system.toml --at home/demo
    local fixture at
    while read -r fixture at; do
        mkdir -p "$root$at"
        mount_fixture "fixtures/$fixture.toml" "$root$at"
    done < <(directive mount)
    mount -t proc proc "$root/proc"
    mount -t devtmpfs devtmpfs "$root/dev"
    mount --bind /dev/pts "$root/dev/pts"
    mount --bind /run/dfdu-shm "$root/dev/shm"
}

# dfdu's history (DFDU_STATE_DIR), out of the scanned tree and out of the disk list, kept
# from one root to the next.
mkdir -p /run/dfdu-shm
mount -t tmpfs -o size=64m,mode=1777 tmpfs /run/dfdu-shm
install -d -o demo -g demo /run/dfdu-shm/dfdu-state

read -r past age < <(directive history) || true
if [ -n "${past:-}" ]; then
    build_root "fixtures/$past.toml"
    # Exit 3: some entries could not be read, as fixtures arrange.
    DEMO_FAKETIME=$(dfdu-demofs now "fixtures/$past.toml" --minus "$age") \
        DEMO_RUN='dfdu scan ~ >/dev/null 2>&1 || [ $? = 3 ]' bash -c 'source /work/env.sh'
    umount -R "$root"
    wait
fi

fixture=$(directive fixture | head -n 1)
fixture=fixtures/${fixture:-home}.toml
build_root "$fixture"

# Read by env.sh, which the tapes source.
DEMO_FAKETIME=$(dfdu-demofs now "$fixture")
export DEMO_FAKETIME
mkdir -p out
vhs "$tape"
