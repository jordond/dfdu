---
title: Overview
description: dfdu is a disk usage browser for the terminal. Browse, sort and delete, and see what is safe to remove.
---

dfdu is a disk usage browser for the terminal. Browse, sort and delete, see what your space is made of (caches, build output, emulators, containers), and get a list of what is safe to remove. It is written in Rust and runs on macOS, Linux and Windows.

![Browsing a home directory, the treemap and the cleanup view](/demos/browse.gif)

## Features

- **Fast.** Parallel scanning tuned to the storage device, and you can start browsing before it finishes.
- **Cleanup view.** Recognises caches, build folders, SDKs and old installers, says how safe each is to delete and how much it frees. [Rules](/docs/rules/) are extendable.
- **Treemap, tree mode, duplicates and largest files**, one key away from the list.
- **Opens last time's scan at once.** A repeat `dfdu PATH` shows the cached scan and refreshes only what changed.
- **History.** Every scan is recorded, so you can ask what grew since last week.
- **Remote.** Browse and delete on another machine over ssh.
- **Scriptable.** JSON and NDJSON output, a CI mode that fails above a size limit, and ncdu export import.
- **Safe by default.** Deletes go to the Trash; permanent deletes ask you to type `delete`. `--read-only` removes the delete keys.

## Similar tools

Other disk usage tools worth a look:

- [ncdu](https://dev.yorhel.nl/ncdu): the classic ncurses disk usage browser, whose keys dfdu follows
- [gdu](https://github.com/dundee/gdu): a fast disk usage analyzer with a console interface, written in Go
- [dua](https://github.com/Byron/dua-cli): disk usage analyzer with an interactive mode, written in Rust
- [Dust](https://github.com/bootandy/dust): a more intuitive version of `du`, written in Rust
- [pdu](https://github.com/KSXGitHub/parallel-disk-usage): parallel disk usage, written in Rust
