# dfdu

A disk usage browser for the terminal, written in Rust. It starts from what `ncdu` does well (a fast, keyboard-driven list of the biggest directories) and adds the things ncdu does not do: a UI that is usable while the scan is still running, parallel scanning tuned to the storage device, categories for what it finds (caches, build outputs, emulators, simulators, containers), and a cleanup view that recommends what to delete and how much space that will actually free.

## Trying it

```sh
cargo run --release -- ~/some/dir          # interactive browser
cargo run --release -- scan ~/some/dir     # headless summary
cargo run --release -- --read-only /       # browse without the delete keys
cargo run --release -- --disks             # choose a disk to browse
cargo run --release -- disks               # list disks with size and free space, like df -h
```

Keys follow ncdu: arrows or `hjkl` to move and open, `s n C M` to sort, `a` apparent size, `r` rescan, `d` move to Trash, `D` delete permanently (asks you to type `delete`), `!` list what could not be read, `2` treemap (`1` back to the list, `+`/`-` depth, `p` colour), `?` help, `q` quit.

Entries that could not be read are marked `!` with the reason (permission denied, removed during the scan, an I/O error, not responding), and directories with such entries below them are marked `.` with a count, as in ncdu. `!` lists them all and jumps to the one you pick. `dfdu scan` prints the reason next to each entry, a breakdown at the end (with a hint to run as root when permissions were the cause), lists every one with `--errors`, and exits with status 3 when anything was not read.

Like `ncdu -x`, a scan stays on the filesystem of the directory it starts in (`-x` is accepted and is the default); mounts below it are listed but not entered. `--cross-mounts` enters other local filesystems. Network filesystems (NFS, SMB, sshfs and other FUSE network mounts) are entered only with `--network`, and kernel filesystems (`/proc`, `/sys`, `/dev`, cgroups) never are. Mounts are classified from the mount table, so a hung network mount is not touched when it is skipped. A directory named on the command line is always scanned, whatever filesystem it is on.

## Development

```sh
cargo test --workspace --release                        # everything, about a minute
DFDU_SIM_SEEDS=20000 cargo test --release -p dfdu-sim   # more simulation seeds
DFDU_SIM_SEED=1234 cargo test --release -p dfdu-sim     # rerun one seed
DFDU_SIM_TRACE=/sim/d0/f1 DFDU_SIM_SEED=1234 cargo test --release -p dfdu-sim -- --nocapture
DFDU_CHECK=1 cargo run --release -- scan /usr           # check every invariant after every slice
```

The simulation (`crates/dfdu-sim`) is the main test of the data pipeline: it runs the real worker, watchdog and applier code against an in-memory filesystem with injected faults, random rescans, deletes and cancels, in a seeded schedule, and compares the result with the filesystem. CI runs 5,000 seeds per suite on every push.
