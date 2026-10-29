# Benchmarks

How long dfdu takes to scan a large tree, and how much memory it uses, next to other disk usage tools: [dua](https://github.com/Byron/dua-cli), [gdu](https://github.com/dundee/gdu), [pdu](https://github.com/KSXGitHub/parallel-disk-usage), [Dust](https://github.com/bootandy/dust), [ncdu](https://dev.yorhel.nl/ncdu) and `du`.

## Summary

Scan time of dfdu against the fastest of the other tools on each test. Lower is better.

| Filesystem | Tree                    | dfdu   | Fastest other tool | dfdu is     |
| ---------- | ----------------------- | ------ | ------------------ | ----------- |
| APFS (Mac) | 1M entries              | 2.70 s | dua, 3.47 s        | 1.3× faster |
| ext4       | 1M entries              | 269 ms | gdu, 333 ms        | 1.2× faster |
| ext4       | 10M entries             | 1.74 s | gdu, 3.51 s        | 2.0× faster |
| ext4       | 10M files in one folder | 1.91 s | ncdu, 9.15 s       | 4.8× faster |
| ZFS        | 1M entries              | 252 ms | gdu, 257 ms        | the same    |
| ZFS        | 10M entries             | 24.0 s | dua, 23.8 s        | the same    |
| ZFS        | 10M files in one folder | 23.7 s | pdu, 24.0 s        | the same    |

On ZFS at 10M entries every parallel tool finishes within a few seconds of the others: the time goes to the filesystem, not the tool (see [ZFS, 10M entries](#zfs-10m-entries)).

### Memory

dfdu keeps the whole tree in memory so you can browse it, sort it and delete from it while and after it scans. That costs about 100-140 MiB for 1M entries and 850 MiB for 10M (690 MiB when they are all files in one folder), or roughly 85 bytes per entry on large trees.

Tools that only print a total (`du`, `dua aggregate`, pdu with `--max-depth 1`, `ncdu -o`) keep no tree and stay in single-digit megabytes. Among tools that do keep one, Dust used 5.2 GiB on the 10M tree and gdu 1.9 GiB on the 10M-file folder.

## Run it yourself

You need [hyperfine](https://github.com/sharkdp/hyperfine) 2.0 or later (for the memory column), and any of the other tools you want to compare on your `PATH`. Then, from a checkout of dfdu:

```sh
cargo xtask bench ~/bench
```

This builds dfdu in release mode, generates a test tree of 1M entries in `~/bench` (once; later runs reuse it), times dfdu and every other tool it finds, and writes a Markdown table to `~/bench/results-*.md`.

| Option        | Default | Meaning                                                          |
| ------------- | ------- | ---------------------------------------------------------------- |
| `--entries N` | 1000000 | Entries in the tree                                              |
| `--wide N`    | 0       | How many of them are files in a single directory                 |
| `--seed N`    | 1       | Seed for the tree generator; the same seed gives the same tree   |
| `--runs N`    | 10      | Runs per tool                                                    |
| `--cold`      | off     | Drop the page cache before every run (Linux, needs root or sudo) |

Without `--cold`, each tool gets two warm-up runs first, so the results are for a warm cache. Some examples:

```sh
cargo xtask bench --entries 10000000 ~/bench                      # the 10M tree
cargo xtask bench --entries 10000000 --wide 9999999 ~/bench       # 10M files in one folder
cargo xtask bench --cold --runs 5 ~/bench                         # cold cache
```

## Results

Each table is sorted from fastest to slowest. Time is the mean ± standard deviation, and "vs dfdu" is how many times longer than dfdu a tool took. CPU is user + system time.

### APFS (Mac), 1M entries

| Tool     | Time              | vs dfdu  | Peak memory | CPU (user + sys) |
| -------- | ----------------- | -------- | ----------- | ---------------- |
| **dfdu** | **2.70 ± 0.08 s** | **1.0×** | 96-99 MiB   | 0.51 + 7.98 s    |
| dua      | 3.47 ± 0.38 s     | 1.3×     | 12-15 MiB   | 0.51 + 10.29 s   |
| dust     | 3.87 ± 0.08 s     | 1.4×     | 574-602 MiB | 1.13 + 28.84 s   |
| pdu -t   | 3.88 ± 0.33 s     | 1.4×     | 11-12 MiB   | 0.78 + 29.34 s   |
| pdu      | 4.13 ± 1.09 s     | 1.5×     | 15-16 MiB   | 0.79 + 29.28 s   |
| gdu      | 5.85 ± 0.23 s     | 2.2×     | 43-47 MiB   | 2.29 + 66.45 s   |
| du (BSD) | 15.8 ± 1.3 s      | 5.9×     | 3 MiB       | 0.08 + 3.79 s    |
| GNU du   | 19.8 ± 0.2 s      | 7.3×     | 4 MiB       | 0.24 + 6.62 s    |
| ncdu     | 19.8 ± 0.2 s      | 7.3×     | 2 MiB       | 0.20 + 6.83 s    |

On APFS every tool spends nearly all of its time in the kernel. dfdu used 8 s of system time and half a second of its own.

10 runs (5 for the last three) after 2 warm-up runs.

### ext4, 1M entries

| Tool        | Time            | vs dfdu  | Peak memory | CPU (user + sys) |
| ----------- | --------------- | -------- | ----------- | ---------------- |
| **dfdu**    | **269 ± 18 ms** | **1.0×** | 105-143 MiB | 0.52 + 1.60 s    |
| gdu         | 333 ± 4 ms      | 1.2×     | 31-34 MiB   | 1.53 + 1.78 s    |
| GNU du      | 840 ± 7 ms      | 3.1×     | 3 MiB       | 0.12 + 0.72 s    |
| ncdu        | 940 ± 23 ms     | 3.5×     | 3 MiB       | 0.21 + 0.73 s    |
| du (uutils) | 1,187 ± 11 ms   | 4.4×     | 107 MiB     | 0.41 + 0.84 s    |
| pdu -t      | 1,372 ± 21 ms   | 5.1×     | 3-4 MiB     | 5.18 + 10.27 s   |
| pdu         | 1,387 ± 15 ms   | 5.2×     | 3-4 MiB     | 5.21 + 10.23 s   |
| dua         | 1,921 ± 21 ms   | 7.1×     | 5-6 MiB     | 4.42 + 3.68 s    |
| dust        | 2,468 ± 24 ms   | 9.2×     | 462-464 MiB | 7.53 + 16.70 s   |

10 runs after 2 warm-up runs.

### ext4, 10M entries

| Tool        | Time              | vs dfdu  | Peak memory | CPU (user + sys) |
| ----------- | ----------------- | -------- | ----------- | ---------------- |
| **dfdu**    | **1.74 ± 0.03 s** | **1.0×** | 847-863 MiB | 4.19 + 13.82 s   |
| gdu         | 3.51 ± 0.02 s     | 2.0×     | 61-66 MiB   | 16.59 + 20.75 s  |
| GNU du      | 9.48 ± 0.06 s     | 5.4×     | 7 MiB       | 1.28 + 8.19 s    |
| ncdu        | 10.4 ± 0.0 s      | 6.0×     | 3 MiB       | 2.08 + 8.29 s    |
| pdu -t      | 12.8 ± 0.3 s      | 7.4×     | 4 MiB       | 49.58 + 94.67 s  |
| pdu         | 12.9 ± 0.1 s      | 7.4×     | 4 MiB       | 50.70 + 94.29 s  |
| du (uutils) | 13.5 ± 0.1 s      | 7.8×     | 800 MiB     | 4.60 + 9.55 s    |
| dua         | 18.0 ± 0.2 s      | 10.3×    | 41 MiB      | 44.14 + 38.86 s  |
| dust        | 24.7 ± 0.2 s      | 14.2×    | 5.1 GiB     | 75.94 + 166.11 s |

Page cache dropped, then 2 warm-up runs and 5 runs per tool.

### ext4, 10M files in one folder

| Tool        | Time              | vs dfdu  | Peak memory | CPU (user + sys) |
| ----------- | ----------------- | -------- | ----------- | ---------------- |
| **dfdu**    | **1.91 ± 0.04 s** | **1.0×** | 693 MiB     | 1.99 + 10.66 s   |
| ncdu        | 9.15 ± 0.02 s     | 4.8×     | 116 MiB     | 1.92 + 7.22 s    |
| pdu -t      | 9.63 ± 0.06 s     | 5.0×     | 406-407 MiB | 30.42 + 51.60 s  |
| pdu         | 9.71 ± 0.03 s     | 5.1×     | 407 MiB     | 30.65 + 52.14 s  |
| du (uutils) | 11.6 ± 0.0 s      | 6.1×     | 1.3 GiB     | 3.56 + 8.00 s    |
| GNU du      | 12.0 ± 0.1 s      | 6.3×     | 31 MiB      | 3.71 + 8.27 s    |
| dua         | 14.8 ± 0.0 s      | 7.7×     | 4 MiB       | 34.87 + 22.15 s  |
| gdu         | 21.1 ± 0.1 s      | 11.0×    | 1.9 GiB     | 17.48 + 10.80 s  |
| dust        | 21.1 ± 0.1 s      | 11.0×    | 2.4 GiB     | 62.30 + 106.30 s |

Page cache dropped, then 2 warm-up runs and 3 runs per tool.

### ZFS, 1M entries

| Tool        | Time            | vs dfdu  | Peak memory | CPU (user + sys) |
| ----------- | --------------- | -------- | ----------- | ---------------- |
| **dfdu**    | **252 ± 68 ms** | **1.0×** | 105-107 MiB | 0.47 + 2.49 s    |
| gdu         | 257 ± 38 ms     | 1.0×     | 42-51 MiB   | 1.67 + 2.93 s    |
| pdu -t      | 1,214 ± 33 ms   | 4.8×     | 4-5 MiB     | 4.74 + 22.90 s   |
| pdu         | 1,247 ± 63 ms   | 4.9×     | 4-5 MiB     | 4.80 + 23.31 s   |
| GNU du      | 1,302 ± 11 ms   | 5.2×     | 3 MiB       | 0.13 + 1.17 s    |
| ncdu        | 1,392 ± 5 ms    | 5.5×     | 2-3 MiB     | 0.21 + 1.18 s    |
| du (uutils) | 1,596 ± 10 ms   | 6.3×     | 107-108 MiB | 0.38 + 1.29 s    |
| dua         | 1,817 ± 18 ms   | 7.2×     | 5-6 MiB     | 4.41 + 4.66 s    |
| dust        | 2,299 ± 73 ms   | 9.1×     | 466-472 MiB | 7.27 + 38.91 s   |

dfdu and gdu are within each other's noise. 10 runs after 2 warm-up runs, with other work running on the machine.

### ZFS, 10M entries

| Tool        | 10M entries      | Peak memory | 10M files in one folder | Peak memory |
| ----------- | ---------------- | ----------- | ----------------------- | ----------- |
| **dfdu**    | **24.0 ± 3.1 s** | 781-833 MiB | **23.7 ± 1.8 s**        | 780-824 MiB |
| dua         | 23.8 ± 4.4 s     | 39-40 MiB   | 25.0 ± 1.4 s            | 4 MiB       |
| pdu -t      | 23.8 ± 4.6 s     | 5 MiB       | 24.0 ± 0.9 s            | 405 MiB     |
| gdu         | 23.8 ± 0.9 s     | 61-83 MiB   | 864 ± 25 s              | 1.5-1.6 GiB |
| pdu         | 26.4 ± 1.9 s     | 5-6 MiB     | 24.4 ± 0.3 s            | 405-406 MiB |
| dust        | 36.4 ± 8.6 s     | 4.6-5.1 GiB | 40.1 ± 3.6 s            | 2.2 GiB     |
| GNU du      | 62.2 ± 0.8 s     | 6-7 MiB     | 82.7 ± 0.3 s            | 31 MiB      |
| ncdu        | 62.7 ± 2.1 s     | 2-3 MiB     | 80.5 ± 0.7 s            | 114 MiB     |
| du (uutils) | 65.9 ± 1.8 s     | 798-799 MiB | 81.5 ± 0.5 s            | 1.3 GiB     |

At this size the parallel tools all take 21-29 s, and nearly all of it is kernel time: dfdu used 247 s of system CPU and 6 s of user CPU on the 10M tree. The ARC was full and evicting throughout. gdu took over 14 minutes on the single folder.

1 warm-up run and 3 runs per tool.

## Test setup

### Machines

| Name | CPU                                                | Memory | Storage                                                                                                       | OS                      | dfdu workers |
| ---- | -------------------------------------------------- | ------ | ------------------------------------------------------------------------------------------------------------- | ----------------------- | ------------ |
| Mac  | Apple M4 Max (12 performance + 4 efficiency cores) | 64 GB  | Internal SSD, APFS                                                                                            | macOS 26.6.2            | 12           |
| ext4 | Intel i5-12400 (6 cores, 12 threads)               | 30 GiB | Crucial P3 2 TB NVMe, ext4 defaults                                                                           | Ubuntu 26.04, Linux 7.0 | 12           |
| ZFS  | Intel i7-13700K (8 + 8 cores, 24 threads)          | 31 GiB | ZFS 2.2.2 pool striped over three NVMe drives (WD SN7100, two Samsung 980 PRO); zstd, atime off, 128K records | Ubuntu 26.04, Linux 6.8 | 16           |

The ext4 machine was otherwise idle. The ZFS machine was not: other work held 18-26 GiB of its memory during the runs, the ARC sat at its 16.7 GB maximum, and the kernel's `arc_prune` thread used a whole CPU at times. Its results are noisier for it.

### Trees

All trees come from `dfdu-bench gen` with seed 1, so anyone can generate the same ones.

| Tree                    | Directories | Files     | Hard links | Symlinks |
| ----------------------- | ----------- | --------- | ---------- | -------- |
| 1M entries              | 46,330      | 939,420   | 9,565      | 4,685    |
| 10M entries             | 477,707     | 9,379,010 | 95,316     | 47,967   |
| 10M files in one folder | 1           | 9,999,999 |            |          |

The 10M trees were run on the Linux machines only.

### Tools and commands

| Tool   | Version                                                        | Command                                                                 |
| ------ | -------------------------------------------------------------- | ----------------------------------------------------------------------- |
| dfdu   |                                                                | `dfdu scan --bytes DIR`                                                 |
| dua    | 2.46.0                                                         | `dua aggregate DIR`                                                     |
| gdu    | 5.38.0                                                         | `gdu -n -p -s DIR`                                                      |
| pdu    | 0.24.0                                                         | `pdu --max-depth 1 DIR`; "pdu -t" adds `--threads` set to the CPU count |
| Dust   | 1.2.6                                                          | `dust -d 0 DIR`                                                         |
| ncdu   | 1.22 (Linux), 2.9.2 (Mac)                                      | `ncdu -0 -o /dev/null DIR`                                              |
| du     | uutils coreutils 0.10.0 (Ubuntu 26.04's default), BSD du (Mac) | `du -s DIR`                                                             |
| GNU du | 9.5 (Linux), 9.12 (Mac)                                        | `du -s DIR`                                                             |

Timed with hyperfine 2.0.0. On Linux the tools are their static release binaries; on the Mac, Homebrew's builds.
