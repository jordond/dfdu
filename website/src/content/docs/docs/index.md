---
title: Overview
description: dfdu is a disk usage browser for the terminal. Browse, sort and delete while the scan runs, and see what is safe to remove.
---

dfdu is a disk usage browser for the terminal. Browse, sort and delete while the scan is still running, see what your space is made of (caches, build output, emulators, containers), and get a list of what is safe to remove. It is written in Rust and runs on macOS, Linux and Windows.

![Browsing a home directory, the treemap and the cleanup view](/demos/browse.gif)

## Features

- **Browse while it scans.** The list fills in and re-sorts as the scan runs. Nothing to wait for.
- **Fast.** Parallel scanning tuned to the storage device. As fast as or faster than gdu and dua in [our benchmarks](/docs/benchmarks/).
- **Cleanup view.** Recognises caches, build folders, SDKs and old installers, says how safe each is to delete and how much it frees. [Rules](/docs/rules/) are extendable.
- **Treemap, tree mode, duplicates and largest files**, one key away from the list.
- **Opens last time's scan at once.** A repeat `dfdu PATH` shows the cached scan and refreshes only what changed.
- **History.** Every scan is recorded, so you can ask what grew since last week.
- **Remote.** Browse and delete on another machine over ssh.
- **Scriptable.** JSON and NDJSON output, a CI mode that fails above a size limit, and ncdu export import.
- **Safe by default.** Deletes go to the Trash; permanent deletes ask you to type `delete`. `--read-only` removes the delete keys.

## Compared to other tools

|                         | dfdu | [ncdu](https://dev.yorhel.nl/ncdu) | [gdu](https://github.com/dundee/gdu) | [dua](https://github.com/Byron/dua-cli) | [Dust](https://github.com/bootandy/dust) |
| ----------------------- | :--: | :--: | :--: | :--: | :--: |
| Interactive browser     | ✓    | ✓    | ✓    | ✓    |      |
| Parallel scan           | ✓    | ✓    | ✓    | ✓    | ✓    |
| Treemap                 | ✓    |      |      |      |      |
| Cleanup recommendations | ✓    |      |      |      |      |
| Duplicate files         | ✓    |      |      |      |      |
| Scan history and diffs  | ✓    |      |      |      |      |
| Reads ncdu exports      | ✓    | ✓    | ✓    |      |      |

Keeping the whole tree in memory costs dfdu about 85 bytes per entry (850 MiB for 10 million files). If you only want a total, `dua aggregate` or `du -s` use a few megabytes. The [benchmarks](/docs/benchmarks/) have the full numbers.

## Next

- [Install dfdu](/docs/install/)
- [Browse a directory](/docs/usage/)
- [Use it from scripts and CI](/docs/cli/)
