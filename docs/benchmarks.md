# Benchmarks

Measured results for ROADMAP M2: scan time and memory against other tools, the applier's
throughput and lock times (D1), and the soak test (`data-pipeline.md` §10.4). Each table says
how it was taken; rerun with the commands in the last section.

## Machine and trees

- 4 vCPUs (Intel Xeon @ 2.10GHz) in a Firecracker VM, 15 GB RAM, virtio disk, Linux 6.18. The
  virtio disk and loop devices report themselves as rotational; dfdu counts virtual disks as
  unknown (4 workers), pdu takes them for spinning disks and uses one thread ("auto").
- Trees from `dfdu-bench gen`, seed 1: 1M entries (46,330 directories, 939,420 files, 9,565 hard
  links, 4,685 symlinks); 10M entries (477,707 directories, 9,379,010 files, 95,316 hard links,
  47,967 symlinks); and one directory of 10M - 1 files (`--wide 10000000`).
- The 1M tree is on the root filesystem (ext4 on the virtio disk). The 10M trees did not fit
  there in inodes, so they are on an ext4 image on a loop device (`mkfs.ext4 -b 1024 -I 256 -N
  10700000 -O inline_data,large_dir`, `losetup --direct-io=on`), 3.6 GB used for either tree.
- Warm cache. At 10M entries "warm" means something else here: the kernel's in-memory inodes
  for 10M files take about 11 GB, so they do not all stay cached; the metadata blocks do. This
  VM also boots with small kernel hash tables (`ihash_entries=65536 dhash_entries=131072`), so
  with millions of cached inodes every lookup walks long hash chains: `perf` showed 63% of the
  scan's CPU time in `find_inode_fast` and 19% in `__d_lookup_rcu`. Every tool pays this per
  file, so 10M times here are several times what a machine with default hash tables would
  give, and the ratios between tools are what to compare.
- Load: other agents were compiling at times. Each table gives the load average; the load
  during a 10M scan includes the scan's own threads (dfdu's 4 workers and applier make it 4-5).
- Tools: dfdu at the commit of this file, dua 2.46.0, gdu 5.29.0, pdu 0.24.0, Dust 1.2.6,
  ncdu 1.19, GNU du 9.4, hyperfine 2.0.0. Commands as in `scripts/bench.sh`, plus pdu with
  `--threads 4`. Memory is the peak RSS hyperfine reports. dua (`aggregate`), pdu with
  `--max-depth 1`, ncdu with `-o` and du keep no tree, so their memory is not comparable with
  dfdu's, gdu's and dust's.

## Scan time and memory

### 1M entries

10 runs per tool after 2 warm-up runs, load 1.3-2.0 (taken after a reboot of the VM, with the
kernel's inode cache holding only this tree).

| Tool | Wall time (mean ± σ, min-max) | Peak memory |
|---|---|---|
| dfdu | 777 ± 59 ms (674-850) | 85 MiB |
| pdu | 1,108 ± 92 ms (1,002-1,319) | 5 MiB |
| pdu --threads 4 | 1,208 ± 100 ms (1,062-1,393) | 5 MiB |
| dust | 1,727 ± 55 ms (1,640-1,797) | 427 MiB |
| gdu | 2,011 ± 105 ms (1,889-2,216) | 1,000 MiB |
| du | 2,414 ± 145 ms (2,226-2,604) | 3 MiB |
| ncdu | 2,574 ± 179 ms (2,310-2,886) | 3 MiB |
| dua | 3,528 ± 121 ms (3,367-3,717) | 9 MiB |

dfdu takes 70% of the time of the fastest other tool (pdu). Interleaved runs of dfdu before and
after the lock-hold changes below: 720 ± 43 ms and 689 ± 25 ms (20 runs each, load 1.8-2.8).

### 10M entries, mixed tree

2 runs per tool, warm (the first run of each tool followed a run of another tool over the same
tree), load 0.7-1.2 before the runs, 1-3 during.

| Tool | Wall time (mean, min-max) | Peak memory |
|---|---|---|
| dfdu | 1.6 min (1.5-1.7) | 764 MiB |
| dust | 1.6 min (1.5-1.8) | 4,707 MiB |
| gdu | 2.1 min (1.5-2.8) | 3,716-7,273 MiB |
| pdu --threads 4 | 2.3 min (2.1-2.5) | 5 MiB |
| dua | 2.6 min (2.3-2.8) | 44 MiB |
| ncdu | 8.6 min (7.1-10.0) | 3 MiB |
| pdu (one thread here) | 9.9 min (8.6-11.3) | 4 MiB |
| du | 10.2 min (10.1-10.2) | 6 MiB |

dfdu's peak is 764 MiB (801 MB) for 10M entries, 80 bytes per entry: the target in
architecture §4 is under 800 MB, so it is at the limit in MB and under it in MiB. The node is 56
bytes; names take 74 MiB.

### 10M entries in one directory

2 runs per tool, load 1.0-1.8 outside the scans. pdu's default ("auto", one thread here) was
measured but its result was lost with the log when the VM restarted.

| Tool | Wall time (mean, min-max) | Peak memory |
|---|---|---|
| dfdu | 2.5 min (2.4-2.5) | 685 MiB |
| dust | 2.5 min (2.4-2.6) | 2,449-2,464 MiB |
| pdu --threads 4 | 2.5 min (2.2-2.8) | 537 MiB |
| dua | 3.1 min (3.0-3.2) | 5 MiB |
| gdu | 6.8 min (4.9-8.6) | 3,299-5,706 MiB |
| ncdu | 8.8 min (7.3-10.3) | 115 MiB |
| du | 11.9 min (11.8-12.0) | 31 MiB |

## Classification (M5)

The rules run on the scan workers (architecture §6). The 1M-entry tree (`dfdu-bench gen`, seed
1), scanned by `dfdu scan --bytes` with the built-in rules and with `--no-rules`, 8 interleaved
runs each, on an M-series Mac (APFS, warm cache, `MacBackend`, 10 workers), load 19-21 from
other work:

| | Wall time (mean, min-max) | User CPU | System CPU |
|---|---|---|---|
| `--no-rules` | 3.46 s (3.29-4.04) | 0.23 s | 20.4 s |
| rules | 3.42 s (3.29-3.57) | 0.34 s | 20.3 s |

Classification costs about 0.11 s of CPU time over the scan, 0.5% of the scan's CPU time; wall
time does not change within the noise. Peak memory goes from 92.8 to 94.1 MiB. The generated
names match few rules, so this measures what every entry pays (the protected-path step, the
name lookups, the CACHEDIR.TAG candidate on every directory), not the cost of matches, which
are rare in real trees too. On `~/dev` (4.1M entries, 490k directories) user time goes from
3.0 to 4.1 s out of about 140 s of CPU time, again with no change in wall time.

Two changes got there: each listing hands its subdirectories the path patterns that can still
reach them (`dfdu_rules::Reach`), so a directory deep in a tree no longer tries every rule and
protected path (this was the largest user-space cost in a profile, 3 µs per listing), and a
glob's depth is computed once instead of on every check.

## Applier (D1)

`examples/apply_rate.rs` records the scanner's messages once and times only the applier
applying them, on one thread (best of the runs in brackets):

| Tree | Rate, glibc malloc | Rate, jemalloc (`LD_PRELOAD`) |
|---|---|---|
| 1M | 6.9-7.7M entries/s (7.66) | 8.6-9.1M entries/s (9.09) |
| 10M mixed | 4.0-5.3M entries/s (5.28), 15 runs | 4.6-6.3M entries/s (6.26), 4 runs |

At 10M the applier alone is under the 5M entries/s threshold in most runs. `perf` on the 10M
runs: `_int_malloc` 16% (10% from `Vec` growth in `dfdu-tree`: each directory's child list and
the arena's vectors), page faults for the 1.2 GB the model grows to (about 300k faults per run),
`LinkTable::reelect` 8% (the final pass over 95k hard links) and `Tree::top` 8% (ancestor walks
that miss the cache). The remedies in `data-pipeline.md` §11 are not where this time goes:
larger blocks and moved name buffers cut per-chunk and per-name copying, but the average chunk
here is 61 entries (one per small directory) and copying names is not in the profile. The
allocator is: with jemalloc the same runs are 20-40% faster. The example also allocates
the next run's model in a heap fragmented by the previous one, which the real program does not.

In `dfdu scan --debug-stats` (applier and 4 workers on 4 CPUs) the rate is entries per second of
lock time, so it includes time the applier spent preempted while holding the lock:
1M: 3.3-5.0M/s; 10M mixed: 1.0-3.8M/s; 10M in one directory: 1.5-3.2M/s.

### After the allocation and walk changes

Commit "Applier at 10M: no allocation per directory or inode, fewer walks": child lists in a
shared arena, directory records and job maps in pages, no per-directory duplicate-filter
allocation, hard-link lists that hold two without allocating, and fused ancestor walks
(`data-pipeline.md` §11.1). Measured with `apply_rate --synth` (the `dfdu-bench` tree served
from memory, `dfdu_bench::synth`), so no 10M tree on disk is needed. Interleaved runs of each
build, 3 rounds of 3-5 runs, load average 1.4-4 (another agent's btrfs timings ran at the same
time):

| | 1M | 10M mixed | 10M in one directory |
|---|---|---|---|
| before | 5.5-8.1M entries/s | 3.4-5.6M | 15-24M |
| after | 6.7-11.2M | 5.2-8.4M (most runs 7-8.4M) | not measured separately (no walks or links there; 15-27M with mimalloc, below) |

Peak memory of the 10M runs (the recorded messages, the plan and one model) fell from 2,657 to
2,476 MiB. A 1M scan of the on-disk tree: 84.3 MiB before, 85.0 MiB after (hyperfine, 5 runs;
wall times not comparable, load 12-14).

Engine mode (`apply_rate --synth N --stat-ns 2000 --engine 4`: the real applier thread and 4
workers, each synthetic entry costing a worker 2 µs of CPU as a warm `statx` does), 2-5 runs
each, load 4-8. "After" here is the committed code plus the page preparation described at the
end of this section, which was then dropped; 128 instead of 512 links per re-election step is
in both:

| | Rate (per second of lock time) | Hold p99 | Slices over 2 ms |
|---|---|---|---|
| 1M, before | 3.8-5.0M entries/s | 1.5-2.05 ms | 0.35-0.98% |
| 1M, after | 5.4-6.5M | 1.28-1.54 ms | 0% |
| 10M mixed, before | 3.9-4.3M | 1.8-3.1 ms | 0.59-1.51% |
| 10M mixed, after | 5.5-5.9M | 1.5-5.1 ms | 0.67-2.21% |
| 10M in one directory, before | 13.0-13.3M | 1.54 ms | 0.15-0.33% |
| 10M in one directory, after | 18.2-19.0M | 1.54 ms | 0.08% |

At 10M the p99 hold depends on the run more than on the build. Per-slice traces of the slow
slices showed two causes: run-queue waits (the applier preempted, 2-6 ms, with the other
agent's processes using 1.5-3 CPUs), and single steps of a few files that took 2-12 ms without
waiting for a CPU, in page faults and `free` (glibc returning memory takes the process's
memory map lock, which the workers' page faults also take).

Tried and not kept:
- mimalloc as the global allocator: the applier alone at 10M 5.2-9.7M entries/s (no faster
  than glibc in most runs), engine mode 4.6-7.9M per second of lock time and p99 1.5-12 ms,
  but a 1M scan peaked at 115 MiB instead of 85 (over the 100 MB target). jemalloc preloaded
  was no faster than glibc with the new code (6.4-7.4M against 6.8-8.2M at 10M). The C library
  also does not build for `aarch64-apple-darwin` here (no SDK).
- Tree pages allocated and touched outside the lock, after each slice, so their page faults
  are not taken while holding it: +4 MiB at 1M (88.9 against 85.0 MiB) and no difference in
  p99 hold that the noise allowed to see.

## Lock hold and wait with 4 workers on 4 CPUs

Before: the code before commit "Workers yield to an applier preempted while holding the write
lock". After: with it and the two commits that followed (yield also while the applier waits
for the lock, 512 links per re-election step). Interleaved runs, `dfdu scan --debug-stats`.
Percentiles are the upper bounds of the histogram buckets (four per power of two).

1M entries, 10 runs each, load 1.8-2.8:

| | Scan wall time | Slices over 2 ms | Hold p99 | Hold max | Wait p99 |
|---|---|---|---|---|---|
| before | 720 ± 43 ms (hyperfine, 20 runs) | 9.7-14.9% | 5.1-6.1 ms | 6.1-8.2 ms | 4.1-8.2 ms |
| after | 689 ± 25 ms | 0-0.7% | 1.5-2.05 ms | 1.8-5.1 ms | 5.1-6.1 ms |

10M mixed, 3 runs each (one more "after" run with per-slice tracing):

| | Scan wall time | Slices over 2 ms | Hold p99 | Hold max |
|---|---|---|---|---|
| before | 90-141 s | 2.9-5.0% | 3.6-33 ms | 20-131 ms |
| after | 116-140 s | 0.7-3.1% | 2.05-33 ms | 33-82 ms |

10M in one directory, 2 runs each:

| | Scan wall time | Slices over 2 ms | Hold p99 | Hold max |
|---|---|---|---|---|
| before | 91-158 s | 4.2-16.7% | 20-41 ms | 49-164 ms |
| after | 127-172 s | 1.9-7.5% | 10-20 ms | 41-115 ms |

The 10M wall times vary by 50% between runs of the same binary (cache state, kernel reclaim of
inodes), so they do not show a difference either way. What made the slow slices: per-slice
`/proc/thread-self/schedstat` showed that before, nearly every slice over 2 ms had waited 2-6
ms on the run queue after one context switch (preempted by a worker); after, at 1M, the rest are
single chunks that took 2-4 ms of CPU. At 10M slow slices remain both ways, still mostly run
queue waits (2-9 ms, with kswapd and the loop device's kernel threads also running) and, in the
one directory, the 10M-entry child list growing.

Tried and not kept: lowering the workers' priority (`setpriority`, no privilege needed) left
7% (nice 2), 5% (nice 5) and 3% (nice 10) of slices over 2 ms at 1M; three workers instead of
four kept slices short (0.4-3% over 2 ms) but scanned 20% slower.

## Soak test (§10.4)

`crates/dfdu-tui/src/soak.rs`: a reader holding the read lock for 5 ms in a loop, the real UI
loop drawing to a terminal blocked for 3 s at a time with 500 ms open in between, totals checked
against an undisturbed scan.

| Tree | Scan (undisturbed) | Terminal blocked | Applier wait p99 (max) | Hold p99 | In transit, max | Process memory |
|---|---|---|---|---|---|---|
| 10M mixed | 118 s (124 s) | 34 times, 101 s | < 8.2 ms (< 24.6 ms) | < 7.2 ms | 1.4 MiB of 64 | 1,252 MiB peak |
| 10M in one directory | 171 s (177 s) | 49 times, 145 s | < 8.2 ms (< 20.5 ms) | < 28.7 ms | 1.2 MiB of 64 | 690 MiB peak |
| 1M, before (4 runs, 1 s blocks) | 0.77-1.12 s | | < 10.2-14.3 ms | < 6.1-7.2 ms | 7-32 MiB | |
| 1M, after (4 runs, 1 s blocks) | 0.79-0.96 s | | < 7.2-10.2 ms | < 1.8-2.6 ms | 4-29 MiB | |

Both 10M runs pass: the scan completes with the reference totals, data in transit stays
within the budget and the applier's p99 wait is under 10 ms. The process memory includes the
two undisturbed scans before it (the mixed tree's 1,252 MiB is what the heap kept after them;
the `dfdu` binary peaks at 764 MiB on the same tree). At 1M one of four "after" runs had its p99
wait in the 8.2-10.2 ms bucket, which the test counts as a failure; the 1M runs have about 200
slices, so p99 is the second-longest wait.

## Running these

```
cargo build --release -p dfdu -p dfdu-bench
target/release/dfdu-bench gen /path/tree-10m --entries 10000000 --seed 1
target/release/dfdu-bench gen /path/wide-10m --entries 10000000 --wide 10000000 --seed 1
scripts/bench.sh --entries 1000000 WORKDIR           # or hyperfine with its commands
hyperfine -N --warmup 2 'target/release/dfdu scan --bytes --no-rules /path/tree-1m' \
    'target/release/dfdu scan --bytes /path/tree-1m'   # classification
target/release/dfdu scan --debug-stats --bytes /path/tree-10m
cargo run --release -p dfdu-engine --example apply_rate -- --runs 5 /path/tree-10m
# The same trees from memory (no disk needed), alone and in the engine with 4 workers:
cargo run --release -p dfdu-engine --example apply_rate -- --runs 3 --synth 10000000
cargo run --release -p dfdu-engine --example apply_rate -- --runs 3 --synth 10000000 --wide 10000000
cargo run --release -p dfdu-engine --example apply_rate -- --runs 2 --synth 10000000 \
    --stat-ns 2000 --engine 4
DFDU_SOAK_TREE=/path/tree-10m cargo test --release -p dfdu-tui soak -- --ignored --nocapture
```

A 10M tree needs 10M inodes and, on ext4 with 1 KiB blocks and inline data, about 3.6 GB; a
directory of 10M entries on ext4 needs `large_dir`. Making either takes 4-11 minutes here.
