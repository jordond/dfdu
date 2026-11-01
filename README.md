<picture>
  <source media="(prefers-color-scheme: dark)" srcset="assets/logo/dfdu-logo-dark.svg">
  <img width="320" src="assets/logo/dfdu-logo.svg" alt="dfdu">
</picture>

A disk usage browser for the terminal. Browse, sort and delete, see what your space is made of (caches, build output, emulators, containers), and get a list of what is safe to remove.

![Browsing a home directory, the treemap and the cleanup view](assets/demos/browse.gif)

## Features

- **Fast.** Parallel scanning tuned to the storage device, and you can start browsing before it finishes. As fast as or faster than gdu and dua in [our benchmarks](https://dfdu.app/docs/benchmarks/).
- **Cleanup view.** Recognises caches, build folders, SDKs and old installers, says how safe each is to delete and how much it frees. [Rules](https://dfdu.app/docs/rules/) are extendable.
- **Treemap, tree mode, duplicates and largest files**, one key away from the list.
- **Opens last time's scan at once.** A repeat `dfdu PATH` shows the cached scan and refreshes only what changed.
- **History.** Every scan is recorded, so you can ask what grew since last week.
- **Remote.** Browse and delete on another machine over ssh.
- **Scriptable.** JSON and NDJSON output, a CI mode that fails above a size limit, and ncdu export import.
- **Safe by default.** Deletes go to the Trash; permanent deletes ask you to type `delete`. `--read-only` removes the delete keys.

## Compared to other tools

|                              | dfdu | [ncdu](https://dev.yorhel.nl/ncdu) | [gdu](https://github.com/dundee/gdu) | [dua](https://github.com/Byron/dua-cli) | [Dust](https://github.com/bootandy/dust) |
| ---------------------------- | :--: | :--: | :--: | :--: | :--: |
| Interactive browser          | ✓    | ✓    | ✓    | ✓    |      |
| Parallel scan                | ✓    | ✓    | ✓    | ✓    | ✓    |
| Treemap                      | ✓    |      |      |      |      |
| Cleanup recommendations      | ✓    |      |      |      |      |
| Duplicate files              | ✓    |      |      |      |      |
| Scan history and diffs       | ✓    |      |      |      |      |
| Reads ncdu exports           | ✓    | ✓    | ✓    |      |      |

Keeping the whole tree in memory costs dfdu about 85 bytes per entry (850 MiB for 10 million files). If you only want a total, `dua aggregate` or `du -s` use a few megabytes. The [benchmarks](https://dfdu.app/docs/benchmarks/) have the full numbers.

## Install

```sh
curl -fsSL https://dfdu.app/install.sh | sh                 # macOS, Linux
brew install jordond/tap/dfdu                               # macOS, Linux
cargo binstall --git https://github.com/jordond/dfdu dfdu
nix run github:jordond/dfdu
```

The install script puts `dfdu` in `~/.local/bin` with its man pages and shell completions, and updates it when run again. Add `-s -- --bin-dir DIR` to install elsewhere or `--version VER` to pin a release; [the install docs](https://dfdu.app/docs/install/) have the details.

Windows:

```powershell
powershell -ExecutionPolicy Bypass -c "irm https://github.com/jordond/dfdu/releases/latest/download/dfdu-installer.ps1 | iex"
```

Binaries for macOS, Linux (static) and Windows are on the [releases page](https://github.com/jordond/dfdu/releases), with man pages and shell completions. To build from source (Rust 1.95+): `cargo install --locked --git https://github.com/jordond/dfdu dfdu`.

## Usage

```sh
dfdu                      # browse the current directory
dfdu ~                    # browse a directory
dfdu --disks              # pick a disk first
dfdu --type video ~       # only video files
dfdu --remote server /srv # another host over ssh (sends dfdu there if needed)
```

Keys follow ncdu. Press `?` in the app for the full list.

| Key | Action | Key | Action |
| --- | --- | --- | --- |
| `↑↓←→` / `hjkl` | Move, open, go back | `1` `2` | List, treemap |
| `s` `n` `C` `M` | Sort by size, name, count, date | `3` | Cleanup |
| `T` | Tree mode | `4` | Duplicates |
| `/` `t` | Filter by name, by file type | `5` | Largest files |
| `v` | Mark a range | `d` | Move to Trash |
| `o` `y` | Open in file manager, copy path | `D` | Delete permanently |
| `r` | Rescan | `q` | Quit |

Unreadable entries are marked `!` with the reason. Press `!` to list them all.

Like `ncdu -x`, a scan stays on one filesystem. Add `--cross-mounts` for other local filesystems and `--network` for network mounts.

Scans that took 3 s or more are cached, so the next `dfdu PATH` opens at once and refreshes in the background (`cached 3 h ago, refreshing` in the header). On macOS, Windows and btrfs only the changed directories are read again. A cache is used for 14 days and the cache keeps at most 1 GiB, readable only by you. `--no-cache` scans afresh, `DFDU_CACHE=0` turns it off, `dfdu cache list` and `dfdu cache clear` manage it.

## Command line

```sh
dfdu scan ~                                 # print a summary
dfdu scan ~ --by-type                       # usage by file type
dfdu disks                                  # disks and free space, like df -h
dfdu dupes ~ --min-size 10MiB               # duplicate files
dfdu report ~/dev                           # what the cleanup rules found
dfdu report ~/dev --format json --fail-above 20GiB   # CI: exit 4 over the limit
dfdu clean ~/dev                            # dry run of the recommended deletes
dfdu clean ~/dev --yes                      # move them to the Trash
dfdu diff ~ --since 7d                      # what grew in the last week
dfdu export ~ --snapshot -o home.snap       # save a scan
dfdu -f home.snap                           # browse a saved scan or an ncdu export
```

`clean` changes nothing without `--yes`. JSON output is versioned; the schemas are in [`schema/`](schema). `dfdu --help` lists every option and exit code.

## Docs

- [Cleanup rules](https://dfdu.app/docs/rules/): the built-in rules and how to write your own
- [Themes](https://dfdu.app/docs/themes/): five built-in themes and custom TOML themes
- [Benchmarks](https://dfdu.app/docs/benchmarks/): scan time and memory against other tools
- [Contributing](CONTRIBUTING.md): building, testing and releasing

## License

[MIT](LICENSE)
