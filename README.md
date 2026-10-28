# dfdu

A disk usage browser for the terminal, written in Rust. It starts from what `ncdu` does well (a fast, keyboard-driven list of the biggest directories) and adds the things ncdu does not do: a UI that is usable while the scan is still running, parallel scanning tuned to the storage device, categories for what it finds (caches, build outputs, emulators, simulators, containers), and a cleanup view that recommends what to delete and how much space that will actually free.

![Browsing a home directory, the treemap and the cleanup view](assets/screenshots/browse.gif)

## Install

Release builds for macOS (Apple silicon and Intel), Linux (static musl, x86_64 and aarch64) and Windows (x86_64) are attached to each [GitHub release](https://github.com/jordond/dfdu/releases), with the man pages and completion scripts.

```sh
brew install jordond/tap/dfdu                                     # macOS and Linux
curl -LsSf https://github.com/jordond/dfdu/releases/latest/download/dfdu-installer.sh | sh
cargo binstall --git https://github.com/jordond/dfdu dfdu         # a release binary through cargo
cargo install --locked --git https://github.com/jordond/dfdu dfdu # from source (Rust 1.95 or later)
nix run github:jordond/dfdu
```

On Windows: `powershell -ExecutionPolicy Bypass -c "irm https://github.com/jordond/dfdu/releases/latest/download/dfdu-installer.ps1 | iex"`. Manifests for Scoop, winget and the AUR (`dfdu-bin`) are in `pkg/`; `scripts/package-manifests.sh VERSION` fills them in from a release.

The release archives carry `man/` and `completions/`; Homebrew puts them under `$(brew --prefix)/share/dfdu`. `dfdu completions SHELL` prints the same scripts and `dfdu man DIR` writes the same pages.

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

## Cleanup, scripts and history

```sh
dfdu report ~/dev                              # what the rules found: caches, build output, dependencies
dfdu report ~/dev --format json --fail-above 20GiB   # for CI: exit 4 over the limit
dfdu clean ~/dev                               # what would be deleted (a dry run)
dfdu clean ~/dev --yes --mode stage            # delete the recommended matches
dfdu scan / --format ndjson > scan.ndjson      # one record per entry
dfdu export ~ --snapshot -o home.snap          # save a scan; dfdu -f home.snap browses it
dfdu diff ~ --since 7d                         # what grew in the last week
dfdu recover                                   # what an interrupted session left behind
```

`report` lists every match of the cleanup rules with its size, age, safety level (`safe`, `regenerable`, `review`, `manual`, `protected`), how it is cleaned up and whether it is recommended. `clean` deletes the recommended matches (or, with `--all`, every match that passes `--category`, `--min-size`, `--older-than` and `--safety`); it changes nothing without `--yes` and runs a tool's own cleanup command (`docker system prune`) only with `--allow-commands`. JSON output carries a `schema_version`; the schemas are in `schema/`. Each scan is recorded in a small SQLite history in the state directory (`--no-history` to skip), which `diff` and the browser's "+1.2 GiB since 2026-10-18" notes compare against. `dfdu --help` lists the exit codes; `dfdu completions SHELL` prints a completion script.

## Development

```sh
cargo test --workspace --release                        # everything, about a minute
DFDU_SIM_SEEDS=20000 cargo test --release -p dfdu-sim   # more simulation seeds
DFDU_SIM_SEED=1234 cargo test --release -p dfdu-sim     # rerun one seed
DFDU_SIM_TRACE=/sim/d0/f1 DFDU_SIM_SEED=1234 cargo test --release -p dfdu-sim -- --nocapture
DFDU_CHECK=1 cargo run --release -- scan /usr           # check every invariant after every slice
```

Fuzzing needs nightly and cargo-fuzz: `cargo +nightly fuzz run snapshot` (targets: `snapshot`, `import`, `rules`, `theme`, `native`, `journal`; seeds in `fuzz/seeds`). `DFDU_CRASH_ROUNDS=40 cargo test -p dfdu --test crash` kills dfdu at random points while it writes and deletes and checks what recovery finds.

The simulation (`crates/dfdu-sim`) is the main test of the data pipeline: it runs the real worker, watchdog and applier code against an in-memory filesystem with injected faults, random rescans, deletes and cancels, in a seeded schedule, and compares the result with the filesystem. CI runs 5,000 seeds per suite on every push.
