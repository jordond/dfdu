# Sourced by tapes/_setup.tape in the recording's shell (root): carries on as the demo user in
# the root run.sh built (chroot), in the fixture's home, with the clock set to the fixture's
# `now` (libfaketime; it moves on from there, and monotonic time is left alone so timers and
# animations run at their speed). FAKETIME is UTC, as `dfdu-demofs now` prints it, and
# libfaketime reads it in TZ. run.sh sets DEMO_RUN to run a command there instead of the shell.
shell='cd ~ && clear && PS1="\w \$ " exec bash --norc --noprofile'
exec env -i \
    HOME=/home/demo USER=demo LOGNAME=demo SHELL=/bin/bash \
    TERM="${TERM:-dumb}" COLORTERM=truecolor LANG=C.UTF-8 TZ=UTC \
    PATH=/opt/dfdu/bin:/usr/bin:/bin \
    DFDU_STATE_DIR=/dev/shm/dfdu-state \
    LD_PRELOAD=/usr/local/lib/libfaketime.so.1 \
    FAKETIME="$DEMO_FAKETIME" FAKETIME_DONT_FAKE_MONOTONIC=1 \
    /usr/sbin/chroot --userspec=1000:1000 --groups=1000 /newroot \
    bash --norc --noprofile -c "${DEMO_RUN:-$shell}"
