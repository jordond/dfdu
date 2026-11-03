---
title: Architecture
description: How a scan flows through dfdu's crates, from the scanner to the browsers, and how the pipeline is tested.
---

This page is for contributors. It follows one scan through the crates in `crates/`, then describes the tests that check the pipeline. Each crate's `lib.rs` (or `main.rs`) opens with a module doc that goes further; start there when you change a crate.

## The pipeline

```text
                 filesystem
                     │
     dfdu-sys ──▶ dfdu-scan          workers and watchdog, one Backend per OS
                     │ ScanMsg
                     ▼
                dfdu-engine          the applier thread, single writer
          ┌──────────┼───────────┐
          ▼          ▼           ▼
     dfdu-tree   dfdu-clean   classifier pool, measurers
     (the Tree)  (deleter)
          │
          ├──▶ dfdu-store        snapshots, scan cache, history, ncdu JSON
          ├──▶ dfdu-dupes        duplicate files, on request
          ▼
     dfdu-session                what a front end shows, no UI types
          │
     ┌────┴─────┐
     ▼          ▼
  dfdu-tui   dfdu-gui            terminal and window
     │
     ▼
   dfdu                          the CLI binary
```

`dfdu-rules` sits beside all of this. The scan workers, the applier and the deleter each read the same `RuleSet`.

`dfdu-tree` has no dfdu dependencies. Everything else builds on it.

## Scanning: dfdu-sys and dfdu-scan

`dfdu-sys` wraps the OS calls that no safe crate provides: the btrfs ioctls and FIEMAP on Linux, `getattrlistbulk` on macOS, and the directory enumeration calls of the Windows backend. On other systems its functions report "not available" rather than fail to build. It also lists disks (`disks`, `Disk`), reads the mount table (`mounts_at`, `MountInfo`) and tells what storage a path is on (`storage`, `Storage`). Its `dirs` module owns where dfdu keeps its files: config, state, cache, crash and rule directories.

`dfdu-scan` is the scanner. It reads directories through the `Backend` trait. There is one backend per platform (`LinuxBackend`, `MacBackend`, `WindowsBackend`) and a `PortableBackend` for everything else. `NativeBackend` names the right one for the build, and `native_backend` sets it up for a given `Storage`. A wrapper that changes one behaviour of a backend implements `Forward` instead.

Workers (`Worker`) take tasks from the shared state (`Shared`), read a directory in blocks, and append what they produce to their own `Outbox`. New subdirectory tasks become stealable only when the outbox is flushed. This gives the one ordering rule the rest of the pipeline depends on: the applier always receives the message naming a directory before any message about it.

What workers send is a `ScanMsg`:

- `Chunk` carries a batch of entries from one directory.
- `Close` comes exactly once per directory that had a task, with its `ListingStatus` and any classes decided once the listing was complete.
- `Stalled` reports a worker stuck on a directory.
- `Quiescent` says every task has had its `Close` flushed.
- `Verified` reports what a resume's check of kept directories found.

The worker and the watchdog are step functions. They have no threads or sleeping inside them. The `runtime` module drives them with real threads. The simulation drives the same code on one thread in a seeded order (see [Testing](#testing)). The `sync` module swaps in `loom`'s atomics and locks under `--cfg loom`.

Workers also classify. The `classify` module checks each entry against the job's rules before the chunk is sent, so categories show up during the scan and the cost is spread over the workers.

## The tree: dfdu-tree

`dfdu-tree` holds the `Tree` of scanned nodes. A node is addressed by `Ix` inside the tree, and by `NodeRef` from outside it. A `NodeRef` stays safe to hold after the node is freed: an answer that names a freed node is dropped. Each node has a `Meta` (with its `Kind`), and directories keep `Totals` and a `DirState`. A node's rule match is a `Class` with a `Safety`.

Only the applier mutates a `Tree`. Everything else reads it under a read lock or holds `NodeRef`s. The `check` module defines the tree's invariants and reports a `Violation` when one breaks. Set `DFDU_CHECK=1` to check every invariant after every applier slice.

`Host` and `HostPath` keep paths as the host the tree was scanned on spells them. That host can be a remote agent or the source of an imported scan, so a Windows host's paths keep their form on a Unix client and the other way round.

## The engine: dfdu-engine

The engine owns the threads and the single writer. `Engine` starts them (`Engine::start` with a backend, `Engine::start_imported` with a loaded tree, `Engine::start_remote` for `--remote`) and is the API the front ends and the headless commands use.

At its centre is the `Applier`. It has no threads either: the runtime's applier loop and the simulation both drive it. Each slice runs under the model's write lock and applies commands, results and scanner messages. The applier handles chunks and closes, pending accounting, jobs, rescans, stalls, quiescence and hard links.

Readers see a `Model`: the tree and the engine `Status`, always consistent with each other. `Engine::read` runs a closure under the read lock. `Engine::changes` is a channel that wakes readers when the model changed. Callers send work with `Engine::send` and a `Command`.

Work too slow for the scan workers runs beside the applier and never touches the tree:

- The classifier pool (`spawn_classifiers`) runs rule guards such as "not tracked by git" or "process not running", and native size commands.
- The measurers (`MeasureReq`, `MeasureResult`) find how much a delete would actually free, using extents where the filesystem shares them.
- The deleter from `dfdu-clean` carries out delete plans.

Each of them gets requests from the applier and sends answers back that name nodes by `NodeRef`.

The `shown` module computes what every front end shows alike: sizes in the chosen `SizeMode`, problems, subvolume labels, rule verdicts, and the mounts the scan did not enter (`NotEntered`). The `crash` module writes a crash report when the applier panics, so callers are not left waiting.

## Persistence: dfdu-store

`dfdu-store` reads and writes trees. It has four parts:

- `snapshot`: dfdu's own format. It is lossless (names are raw bytes), zstd-compressed in frames with a CRC32C each, and ends with a footer that counts everything.
- `cache`: the browser's last complete scan of a directory, kept as a snapshot. The next `dfdu PATH` opens it at once and rescans in the background. Files are keyed by a hash of the path and scan options (`Key`).
- `history`: a SQLite database with one row per scan and the sizes of the top levels, the big directories and every rule match. It is written after a scan finishes, in one transaction. `dfdu diff` and the browser's growth since the last scan read it.
- `ncdu`: ncdu's JSON format, both directions, streaming.

Files are written with `write_atomic`, so a crash leaves the previous file intact.

## Rules and cleanup: dfdu-rules, dfdu-clean, dfdu-dupes

`dfdu-rules` defines the categories (`Category`) and the cleanup rules. Rules are TOML. A built-in pack ships in the binary (`builtin`), and users add or override rules in their rule directory (`load`). `RuleSet::classify` decides from one directory listing which rule an entry matches. Conditions on an entry's own contents are checked when its listing arrives (`RuleSet::check_inside`). The `protected` module lists paths fixed in the code that win over every rule. The format is documented in [Cleanup rules](/docs/rules/).

`dfdu-clean` deletes. The applier builds a plan, and the deleter thread (`spawn_deleter`, `DeleterHandle`) runs it. Before an item is touched, `verify` checks it against the scan and against the protected paths, and may return a `Rejection`. Items go to the trash, to staging (with undo inside `STAGE_UNDO_WINDOW`) or are removed for good. A per-session `Journal` writes each intent to disk before the item is touched. The `recover` module reads the journals of sessions that have exited and lists what they left unfinished. `dfdu recover` is its command.

`dfdu-dupes` finds duplicate files on request. `candidates` picks files from the tree under the read lock: regular files above a minimum size, each inode once, and no cloud placeholders. `find` then reads them without any lock, in three rounds: equal size, a hash of the first and last bytes, and a SHA-256 of the whole file. `Group::wasted` counts only data that is not shared by clones or reflinks.

## The front ends: dfdu-session, dfdu-tui, dfdu-gui

`dfdu-session` is what a front end shows besides drawing and input. It has no terminal or window types. The `Session` holds the engine, the browser's `SessionState` and the jobs beside them: the scan history, the scan cache and lookups outside the scan. A front end's loop waits on input, on `Session::changes` and until `Tick::wake_at`, then calls `Session::tick`. The tick reads the model when due. It copies a directory snapshot (`snapshot`) and what the features need (`extras`) under the read lock, then releases it. Reads are paced so the read lock is held at most a tenth of the time. Time is passed in, so the pacing follows the caller's clock. A front end implements `FrontEnd` for what it keeps around the state.

The session also holds the logic of the views (`treemap`, `cleanup`, `dupes`, `largest`), the marks, filters, actions and their keys (`actions`), and the theme's tokens (`palette`). Both front ends share them.

`dfdu-tui` is the terminal browser. Its run loop (`run_loop`) ticks the session and draws from the copy. It redraws at once on input, at most 10 times a second while the engine reports changes, and never when idle. State and keys are in `app`, drawing in `view`, and each feature has its own module under `features` or `overlay`.

`dfdu-gui` is the same browser in a window, on the same session driver. It draws with wgpu, or with OpenGL when wgpu cannot start. A thread (`bridge`) asks for a frame when the engine's model changes. See [Desktop app](/docs/desktop/) for how it is used.

## The CLI: crates/dfdu

`crates/dfdu` is the `dfdu` binary. With no subcommand it opens the terminal browser. The subcommands are headless and use the engine directly: `scan`, `disks`, `report`, `clean`, `diff`, `history`, `dupes`, `export`, `import`, `recover`, `cache`, `man` and `completions`. `agent` is the other end of `--remote`: it scans on the remote host and writes results to stdout in dfdu's wire format (`dfdu_engine::wire`). See [Command line](/docs/cli/) for usage.

## Where a change goes

- A built-in cleanup rule: `crates/dfdu-rules/src/builtin.toml`.
- A new `Backend` method: the trait in `dfdu-scan`, its `Forward` impl, and each native backend (or `portable_queries!` when they answer like the portable one).
- A change to what the tree holds: `dfdu-tree`, the applier in `dfdu-engine`, and the `snapshot` format in `dfdu-store`.
- A view or action both browsers show: its logic in `dfdu-session`, then drawing and keys in `dfdu-tui` and `dfdu-gui`.
- A headless command: a module in `crates/dfdu/src` and a variant in its command list.

## Testing

`cargo xtask` runs every suite below. `CONTRIBUTING.md` lists the commands.

### Simulation: dfdu-sim

`dfdu-sim` is the main test of the data pipeline. It builds an in-memory filesystem from a `Spec` and scans it with the real `Worker`, watchdog and `Applier` code through a `SimBackend`. Everything runs on one thread with virtual time (`SimClock`), in an order chosen by a seeded `Rng`. The run also mixes in random rescans, deletes and cancels.

Each directory in the spec can carry a `Fault`: permission denied, EIO after some entries, a panic at a block or on open, or a read or stat that stalls for a while or forever. After every applier step the tree invariants are checked. At the end every directory's totals are compared with a reference computed straight from the spec.

A failure prints its seed. `cargo xtask sim --seed N` reruns it, and `--trace PATH` follows one path through it. Scenario tests in `crates/dfdu-sim/tests` build their own spec, force the steps they care about, then call `Sim::run_to_end`. CI runs 5,000 seeds per suite on every push.

### Hung mounts: dfdu-hangfs

`dfdu-hangfs` is a FUSE filesystem that runs inside the test process. Selected directories never answer `readdir` and selected files never answer `getattr`, like a hung network mount. The rest answers normally, optionally after a delay. `Mounted` answers every kept request with `EIO` on release or drop, so blocked threads return. It needs `/dev/fuse`; tests skip with a message when it is missing. The engine and CLI `fuse` tests use it.

### Loom

The scanner's hand-written synchronisation is checked with `loom`. `crates/dfdu-scan/tests/loom.rs` runs the real `Shared`, `Worker`, watchdog round and `Budget` on `loom`'s atomics and locks. `crates/dfdu-engine/tests/loom.rs` checks the applier's wake to readers. Run them with `cargo xtask loom`, which builds in `target/loom` with `--cfg loom`.

### Fuzzing

`fuzz/` holds cargo-fuzz targets for the parsers that read outside input: ncdu import, snapshots, the deletion journal, the remote wire format, rule files, themes, native command output and NTFS records. Each target runs from its corpus plus `fuzz/seeds` and `fuzz/regressions`. Fuzzing needs nightly, or the `dfdu-fuzz` Docker image.

### Crash tests

`crates/dfdu/tests/crash.rs` kills `dfdu` with SIGKILL at random points while it writes a snapshot, records a scan in the history, or deletes. After each kill the previous snapshot must still load, the history must open, and `dfdu recover` must report every half-done item. `cargo xtask crash --rounds N` runs more rounds. The seed is printed; `DFDU_CRASH_SEED` repeats a run.

Panics are covered too. `crates/dfdu-engine/tests/crash.rs` checks that an applier panic is reported and written to a crash file. `crates/dfdu/tests/worker_panic.rs` checks that a panic in a scan worker is caught and the directory reported as unreadable. `crates/dfdu/tests/ui_panic.rs` checks that a panic on the UI thread restores the terminal and writes a crash report.
