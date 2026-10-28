# Benchmarks

Measured results, scan time and memory against other tools.

## Machines and trees

- **Mac**: Apple M4 Max (12 performance and 4 efficiency cores), 64 GB, internal SSD, APFS,
  macOS 26.6.2. dfdu runs 12 workers (one per performance core).
- **ext4**: Intel i5-12400 (6 cores, 12 threads), 30 GiB, Crucial P3 2 TB NVMe, ext4
  (defaults), Ubuntu 26.04, Linux 7.0. dfdu runs 12 workers. Otherwise idle.
- **ZFS**: Intel i7-13700K (8+8 cores, 24 threads), 31 GiB, a ZFS pool striped over three NVMe
  drives (WD SN7100, two Samsung 980 PRO), zstd compression, atime off, 128K records, ZFS 2.2.2,
  Ubuntu 26.04, Linux 6.8. dfdu runs 16 workers (the cap). Other work on this machine held 18-26
  GiB of its memory during the runs, the ARC sat at its 16.7 GB maximum, and the kernel's
  `arc_prune` thread used a whole CPU at times.
- Trees from `dfdu-bench gen`, seed 1: 1M entries (46,330 directories, 939,420 files, 9,565
  hard links, 4,685 symlinks); 10M entries (477,707 directories, 9,379,010 files, 95,316 hard
  links, 47,967 symlinks); one directory of 10M - 1 files (`--wide 10000000`). The 10M trees
  are on the Linux machines only. Generating them took 41-130 s.
- Tools: dua 2.46.0, gdu 5.38.0, pdu 0.24.0, Dust 1.2.6, ncdu 1.22 (Linux) and 2.9.2 (Mac),
  GNU du 9.5 (Linux, `gnudu`) and 9.12 (Mac), hyperfine 2.0.0. On Linux, the static release
  binaries; on the Mac, Homebrew's. Ubuntu 26.04's `du` is uutils coreutils 0.10.0 and the
  Mac's is BSD du; both are listed as "du". Commands as in `scripts/bench.sh`, plus pdu with
  `--threads` set to the CPU count ("pdu -t") and GNU du.
- Memory: dua (`aggregate`), pdu with `--max-depth 1`, ncdu with `-o` and GNU du keep no tree.
  gdu 5.38 with `-n -p -s` keeps much less than 5.29 did (61-66 MiB at 10M mixed, against 3.6-7.1
  GiB before) except in the one large directory (1.9 GiB). uutils du keeps something per entry:
  800 MiB at 10M mixed, 1.3 GiB in the one directory.

### Mac, 1M entries

10 runs (5 for the last three) after 2 warm-up runs (1), load 3.9 before and 3.0-15.1 during.

| Tool     | Wall time (mean ± σ, min-max) | Peak memory | User / system CPU |
| -------- | ----------------------------- | ----------- | ----------------- |
| dfdu     | 2.70 ± 0.08 s (2.60-2.83)     | 121-123 MiB | 0.51 / 7.98 s     |
| dua      | 3.47 ± 0.38 s (2.87-4.23)     | 12-15 MiB   | 0.51 / 10.29 s    |
| dust     | 3.87 ± 0.08 s (3.79-4.05)     | 574-602 MiB | 1.13 / 28.84 s    |
| pdu -t   | 3.88 ± 0.33 s (3.63-4.80)     | 11-12 MiB   | 0.78 / 29.34 s    |
| pdu      | 4.13 ± 1.09 s (3.67-7.23)     | 15-16 MiB   | 0.79 / 29.28 s    |
| gdu      | 5.85 ± 0.23 s (5.49-6.23)     | 43-47 MiB   | 2.29 / 66.45 s    |
| du (BSD) | 15.8 ± 1.3 s (15.1-18.2)      | 3 MiB       | 0.08 / 3.79 s     |
| GNU du   | 19.8 ± 0.2 s (19.5-20.0)      | 4 MiB       | 0.24 / 6.62 s     |
| ncdu     | 19.8 ± 0.2 s (19.7-20.1)      | 2 MiB       | 0.20 / 6.83 s     |

dfdu takes 78% of the time of the fastest other tool (dua). APFS makes every tool spend its
time in the kernel: dfdu's 8 s of system time against 0.5 s of user time.

### ext4, 1M entries

10 runs after 2 warm-up runs, load 0.3 before and 3.2 after.

| Tool        | Wall time (mean ± σ, min-max) | Peak memory | User / system CPU |
| ----------- | ----------------------------- | ----------- | ----------------- |
| dfdu        | 269 ± 18 ms (239-286)         | 105-143 MiB | 0.52 / 1.60 s     |
| gdu         | 333 ± 4 ms (327-339)          | 31-34 MiB   | 1.53 / 1.78 s     |
| GNU du      | 840 ± 7 ms (828-855)          | 3 MiB       | 0.12 / 0.72 s     |
| ncdu        | 940 ± 23 ms (926-1,004)       | 3 MiB       | 0.21 / 0.73 s     |
| du (uutils) | 1,187 ± 11 ms (1,172-1,204)   | 107 MiB     | 0.41 / 0.84 s     |
| pdu -t      | 1,372 ± 21 ms (1,344-1,412)   | 3-4 MiB     | 5.18 / 10.27 s    |
| pdu         | 1,387 ± 15 ms (1,357-1,410)   | 3-4 MiB     | 5.21 / 10.23 s    |
| dua         | 1,921 ± 21 ms (1,894-1,960)   | 5-6 MiB     | 4.42 / 3.68 s     |
| dust        | 2,468 ± 24 ms (2,431-2,507)   | 462-464 MiB | 7.53 / 16.70 s    |

### ext4, 10M entries, mixed tree

Page cache dropped, then 2 warm-up runs and 5 runs per tool, load 3.9 before and 1.1 after.

| Tool        | Wall time (mean ± σ, min-max) | Peak memory     | User / system CPU |
| ----------- | ----------------------------- | --------------- | ----------------- |
| dfdu        | 1.74 ± 0.03 s (1.72-1.79)     | 847-863 MiB     | 4.19 / 13.82 s    |
| gdu         | 3.51 ± 0.02 s (3.49-3.54)     | 61-66 MiB       | 16.59 / 20.75 s   |
| GNU du      | 9.48 ± 0.06 s (9.42-9.57)     | 7 MiB           | 1.28 / 8.19 s     |
| ncdu        | 10.4 ± 0.0 s (10.3-10.4)      | 3 MiB           | 2.08 / 8.29 s     |
| pdu -t      | 12.8 ± 0.3 s (12.6-13.1)      | 4 MiB           | 49.58 / 94.67 s   |
| pdu         | 12.9 ± 0.1 s (12.8-13.0)      | 4 MiB           | 50.70 / 94.29 s   |
| du (uutils) | 13.5 ± 0.1 s (13.4-13.6)      | 800 MiB         | 4.60 / 9.55 s     |
| dua         | 18.0 ± 0.2 s (17.8-18.4)      | 41 MiB          | 44.14 / 38.86 s   |
| dust        | 24.7 ± 0.2 s (24.5-25.0)      | 5,259-5,261 MiB | 75.94 / 166.11 s  |

dfdu takes half the time of gdu.

### ext4, 10M entries in one directory

Page cache dropped, then 2 warm-up runs and 3 runs per tool, load 1.1 before and 1.4 after.

| Tool        | Wall time (mean ± σ, min-max) | Peak memory     | User / system CPU |
| ----------- | ----------------------------- | --------------- | ----------------- |
| dfdu        | 2.01 ± 0.10 s (1.93-2.12)     | 843-844 MiB     | 2.03 / 10.82 s    |
| ncdu        | 9.15 ± 0.02 s (9.13-9.17)     | 116 MiB         | 1.92 / 7.22 s     |
| pdu -t      | 9.63 ± 0.06 s (9.56-9.68)     | 406-407 MiB     | 30.42 / 51.60 s   |
| pdu         | 9.71 ± 0.03 s (9.68-9.73)     | 407 MiB         | 30.65 / 52.14 s   |
| du (uutils) | 11.6 ± 0.0 s (11.5-11.6)      | 1,334 MiB       | 3.56 / 8.00 s     |
| GNU du      | 12.0 ± 0.1 s (11.9-12.1)      | 31 MiB          | 3.71 / 8.27 s     |
| dua         | 14.8 ± 0.0 s (14.8-14.9)      | 4 MiB           | 34.87 / 22.15 s   |
| gdu         | 21.1 ± 0.1 s (21.0-21.2)      | 1,936-1,940 MiB | 17.48 / 10.80 s   |
| dust        | 21.1 ± 0.1 s (21.0-21.3)      | 2,477-2,478 MiB | 62.30 / 106.30 s  |

dfdu takes 22% of the time of the fastest other tool (ncdu).

### ZFS, 1M entries

10 runs after 2 warm-up runs, load 4.1 before and 6.9 after (other work on the machine).

| Tool        | Wall time (mean ± σ, min-max) | Peak memory | User / system CPU |
| ----------- | ----------------------------- | ----------- | ----------------- |
| dfdu        | 252 ± 68 ms (200-442)         | 105-107 MiB | 0.47 / 2.49 s     |
| gdu         | 257 ± 38 ms (224-361)         | 42-51 MiB   | 1.67 / 2.93 s     |
| pdu -t      | 1,214 ± 33 ms (1,154-1,274)   | 4-5 MiB     | 4.74 / 22.90 s    |
| pdu         | 1,247 ± 63 ms (1,140-1,362)   | 4-5 MiB     | 4.80 / 23.31 s    |
| GNU du      | 1,302 ± 11 ms (1,283-1,321)   | 3 MiB       | 0.13 / 1.17 s     |
| ncdu        | 1,392 ± 5 ms (1,384-1,401)    | 2-3 MiB     | 0.21 / 1.18 s     |
| du (uutils) | 1,596 ± 10 ms (1,579-1,610)   | 107-108 MiB | 0.38 / 1.29 s     |
| dua         | 1,817 ± 18 ms (1,792-1,844)   | 5-6 MiB     | 4.41 / 4.66 s     |
| dust        | 2,299 ± 73 ms (2,205-2,419)   | 466-472 MiB | 7.27 / 38.91 s    |

dfdu and gdu tie within the noise.

### ZFS, 10M entries

1 warm-up run and 3 runs per tool. Load 1.1 before and 1.9 after the mixed tree; 1.9 before
and 1.7 after the one directory.

| Tool        | Mixed tree               | Peak memory     | One directory            | Peak memory     |
| ----------- | ------------------------ | --------------- | ------------------------ | --------------- |
| dfdu        | 24.0 ± 3.1 s (21.7-27.5) | 781-833 MiB     | 23.7 ± 1.8 s (21.9-25.5) | 780-824 MiB     |
| dua         | 23.8 ± 4.4 s (20.7-28.8) | 39-40 MiB       | 25.0 ± 1.4 s (23.4-26.0) | 4 MiB           |
| pdu -t      | 23.8 ± 4.6 s (20.3-29.0) | 5 MiB           | 24.0 ± 0.9 s (23.2-24.9) | 405 MiB         |
| gdu         | 23.8 ± 0.9 s (22.8-24.5) | 61-83 MiB       | 864 ± 25 s (835-880)     | 1,539-1,598 MiB |
| pdu         | 26.4 ± 1.9 s (24.4-28.1) | 5-6 MiB         | 24.4 ± 0.3 s (24.2-24.6) | 405-406 MiB     |
| dust        | 36.4 ± 8.6 s (30.7-46.3) | 4,744-5,267 MiB | 40.1 ± 3.6 s (36.1-43.0) | 2,212-2,275 MiB |
| GNU du      | 62.2 ± 0.8 s (61.4-62.9) | 6-7 MiB         | 82.7 ± 0.3 s (82.5-83.0) | 31 MiB          |
| ncdu        | 62.7 ± 2.1 s (60.4-64.6) | 2-3 MiB         | 80.5 ± 0.7 s (79.7-80.9) | 114 MiB         |
| du (uutils) | 65.9 ± 1.8 s (64.4-67.8) | 798-799 MiB     | 81.5 ± 0.5 s (81.1-82.1) | 1,332-1,333 MiB |

At 10M on this machine the filesystem sets the time, not the tool: the parallel tools all take
21-29 s and spend it in the kernel (dfdu: 247 s of system CPU and 6 s of user CPU on the mixed
tree), with the ARC full and evicting. gdu in the one directory spent 326 s of CPU in each
run of 864 s.
