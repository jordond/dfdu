---
title: Browsing
description: Open a directory in dfdu, the keys, the views, mounts and the scan cache.
---

```sh
dfdu                      # browse the current directory
dfdu ~                    # browse a directory
dfdu --disks              # pick a disk first
dfdu --type video ~       # only video files
dfdu --remote server /srv # another host over ssh (dfdu installed there)
```

## Keys

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

## Views

![The cleanup view](/demos/cleanup.gif)

The cleanup view (`3`) lists what the [cleanup rules](/docs/rules/) found, with how safe each match is to delete and how much it frees. The treemap (`2`), tree mode (`T`), duplicates (`4`) and largest files (`5`) are one key away from the list.

## Mounts

Like `ncdu -x`, a scan stays on one filesystem. Add `--cross-mounts` for other local filesystems and `--network` for network mounts.

## The scan cache

Scans that took 3 s or more are cached, so the next `dfdu PATH` opens at once and refreshes in the background (`cached 3 h ago, refreshing` in the header). On macOS, Windows and btrfs only the changed directories are read again.

A cache is used for 14 days and the cache keeps at most 1 GiB, readable only by you. `--no-cache` scans afresh, `DFDU_CACHE=0` turns it off, `dfdu cache list` and `dfdu cache clear` manage it.

## Deleting

Deletes go to the Trash. Permanent deletes (`D`) ask you to type `delete`. `--read-only` removes the delete keys.
