#!/usr/bin/env python3
"""Runs the dut browser in a pseudo-terminal and times it from outside (ROADMAP M1).

Measures, per run:
- start to the first frame with entries: from just before the process is started to the first
  output byte of an entry row (a percentage; the header has none);
- sort key to frame: from writing `n` or `s` to the output that shows the new sort key in the
  header ("nam" or "siz", which ratatui writes as one run of changed cells).

Keys alternate between `n` and `s`; the next key is written 10-40 ms after the previous one was
shown, until the header says "done" or 5 s pass. This is a cross-check of
crates/dut-tui/tests/latency.rs, which drives the same loop in process with more kinds of keys.

Usage: scripts/pty-latency.py DUT TREE [RUNS]
"""

import fcntl
import os
import pty
import random
import select
import struct
import sys
import termios
import time


def read_until(fd, buf, start, needle, deadline):
    """Reads output into buf until needle appears at or after offset start."""
    while True:
        i = buf.find(needle, start)
        if i >= 0:
            return i
        left = deadline - time.monotonic()
        if left <= 0:
            return -1
        r, _, _ = select.select([fd], [], [], left)
        if r:
            try:
                data = os.read(fd, 65536)
            except OSError:
                return -1
            if not data:
                return -1
            buf.extend(data)


def drain(fd, buf):
    while select.select([fd], [], [], 0)[0]:
        try:
            data = os.read(fd, 65536)
        except OSError:
            return
        if not data:
            return
        buf.extend(data)


def run(dut, tree, rng):
    t0 = time.monotonic()
    pid, fd = pty.fork()
    if pid == 0:
        fcntl.ioctl(0, termios.TIOCSWINSZ, struct.pack("HHHH", 40, 120, 0, 0))
        os.execv(dut, [dut, tree])
    buf = bytearray()
    if read_until(fd, buf, 0, b"%", t0 + 30) < 0:
        raise SystemExit("no data frame")
    first = time.monotonic() - t0
    lat = []
    end = time.monotonic() + 5
    key = b"n"
    while time.monotonic() < end and b"done" not in buf:
        time.sleep(rng.uniform(0.010, 0.040))
        drain(fd, buf)
        mark = len(buf)
        sent = time.monotonic()
        os.write(fd, key)
        want = b"nam" if key == b"n" else b"siz"
        if read_until(fd, buf, mark, want, sent + 10) < 0:
            raise SystemExit(f"key {key} not shown")
        lat.append(time.monotonic() - sent)
        key = b"s" if key == b"n" else b"n"
    os.write(fd, b"q")
    os.waitpid(pid, 0)
    os.close(fd)
    return first, lat


def pct(v, p):
    v = sorted(v)
    i = max(1, min(len(v), -(-len(v) * p // 100)))
    return v[int(i) - 1]


def main():
    dut, tree = sys.argv[1], sys.argv[2]
    runs = int(sys.argv[3]) if len(sys.argv) > 3 else 10
    rng = random.Random(1)
    run(dut, tree, rng)  # warms the cache
    firsts, lats = [], []
    for _ in range(runs):
        first, lat = run(dut, tree, rng)
        firsts.append(first)
        lats.extend(lat)
    ms = lambda d: f"{d * 1000:6.1f} ms"
    for name, v in (("start to first data frame", firsts), ("sort key to frame", lats)):
        print(f"{name}: n={len(v)} p50 {ms(pct(v, 50))} p99 {ms(pct(v, 99))} max {ms(max(v))}")
    print("load:", open("/proc/loadavg").read().split()[:3])


if __name__ == "__main__":
    main()
