---
title: Desktop app
description: The dfdu desktop app for macOS, Linux and Windows. Install it, scan a disk or folder, clean up, and scan as an administrator.
---

The desktop app, `dfdu-gui`, is the dfdu browser in a window. It shares its scanner, cleanup rules, delete checks, undo and scan history with `dfdu` in the terminal, and has the same five views: Files, Map, Cleanup, Duplicates and Largest. It ships with every release from 0.3.0 and runs on macOS, Linux and Windows.

![The treemap of a home directory in the desktop app](/gui/map.png)

## Install

### macOS

With Homebrew:

```sh
brew install --cask jordond/tap/dfdu-gui
```

Or with the install script, which installs the CLI and puts `dfdu.app` in `~/Applications`:

```sh
curl -fsSL https://dfdu.app/install.sh | sh -s -- --gui
```

Or download the disk image from the [latest release](https://github.com/jordond/dfdu/releases/latest) and drag dfdu into Applications: `dfdu-gui-aarch64-apple-darwin.dmg` for Apple silicon, `dfdu-gui-x86_64-apple-darwin.dmg` for Intel.

### Linux

x86_64 only. The install script installs the CLI and the AppImage as `dfdu-gui` next to `dfdu`, with a desktop entry so it shows up in your app launcher:

```sh
curl -fsSL https://dfdu.app/install.sh | sh -s -- --gui
```

The [latest release](https://github.com/jordond/dfdu/releases/latest) also has a `.deb` for Debian and Ubuntu, which installs `/usr/bin/dfdu-gui`:

```sh
sudo apt install ./dfdu-gui-x86_64-unknown-linux-gnu.deb
```

The AppImage needs FUSE 2. If it does not start, install `libfuse2` (Debian, Ubuntu; `libfuse2t64` on Ubuntu 24.04), `fuse2` (Arch) or `fuse-libs` (Fedora), or run it with `--appimage-extract-and-run`.

### Windows

From the [latest release](https://github.com/jordond/dfdu/releases/latest), run `dfdu-gui-x86_64-pc-windows-msvc-setup.exe`. It installs dfdu for your user in `%LOCALAPPDATA%\dfdu` without administrator rights, and adds a Start menu shortcut and an uninstaller. To skip installing, unzip `dfdu-gui-x86_64-pc-windows-msvc.zip` and run `dfdu-gui.exe`.

Every file on the release page has a `.sha256` checksum next to it.

## Choosing what to scan

Opened without a folder, the app shows a start screen with your disks and how full each one is. Pick a disk with the arrow keys and press Enter, or press its number. You can also open a folder with Cmd-O (Ctrl-O on Linux and Windows), type its path, or drop it anywhere on the window. **Open file** browses a scan saved with `dfdu export` or an ncdu export.

From a terminal, pass the folder instead:

```sh
dfdu-gui ~/Downloads
```

On macOS the app is `dfdu.app`; run `open -a dfdu --args ~/Downloads` to pass it a folder.

As in the terminal, you can browse while the scan is still running, and a folder you scanned before opens from its cached scan at once and refreshes in the background.

## Views

The tabs at the top switch views, or press `1` to `5`.

- **Files** (`1`): the entries of the current folder with size bars. Sort from the column headers. Tick **Tree** to open folders in place.
- **Map** (`2`): the current folder as a treemap. Hover a tile for its size, click to select it, double-click to zoom in. Colour tiles by category, age or top folder, and change the depth with `+` and `-`.
- **Cleanup** (`3`): what the [cleanup rules](/docs/rules/) found in the whole scan, grouped by category and safety level. The first time you open it, the recommended items are ticked for you.
- **Duplicates** (`4`): identical files, compared in the background with a progress bar and a cancel button. One copy in each group is kept and never ticked; press `K` to keep a different copy.
- **Largest** (`5`): the biggest files below the current folder, wherever they are.

![The cleanup view, grouped by category and safety level](/gui/cleanup.png)

![The duplicates view, with one copy of each file kept](/gui/dupes.png)

Tick **Details**, or press `i`, for a panel on the right with what the selected entry is, which rule matched it, what you lose by deleting it, how much it grew since your last scan, and its largest children.

![The file list with the details panel open](/gui/files.png)

Where a tool has its own cleanup command, such as `docker system prune`, select the item in Cleanup and press `N`. dfdu runs the command's dry run first and only offers the real run after it succeeds.

## Deleting

Mark entries with Space, or tick them in Cleanup and Duplicates, then press `d` (or Delete) to move them to the Trash, or `D` to delete them permanently. Right-click a row for the same actions.

The delete dialog lists the items, their total size, and how much space dfdu expects to free. It opens with Cancel selected, so pressing Enter by accident deletes nothing. Permanent deletes, and moves to the Trash of 50 GiB or more, ask you to type `delete` first.

![The delete dialog](/gui/delete.png)

The dialog also offers staging: the items are moved to a staging folder at once and deleted a minute later. `U` puts back the last delete, from the Trash or, within that minute, from staging.

To browse with no way to delete, start the app with `--read-only`.

## Keys

The keys are the same as in the terminal browser: `j` and `k` move, Enter opens a folder, `s` sorts by size and `/` filters. Press `?` for the full list, with a search box.

Cmd-K (Ctrl-K on Linux and Windows) opens the command palette, which lists every action and theme with its key. A few desktop shortcuts work too:

| Shortcut | Action |
| --- | --- |
| Cmd-K / Ctrl-K | Command palette |
| Cmd-F / Ctrl-F | Filter this folder |
| Cmd-C / Ctrl-C | Copy the selected path |
| Delete | Move to Trash |
| Cmd-W / Ctrl-W | Close the window |

## Scanning as an administrator

Some folders can only be read as an administrator, and a normal scan counts them as unreadable. On macOS and Linux, turn on the **Full disk access** switch on the start screen to scan them too. How that works depends on the system:

- **macOS:** the app installs a helper that scans as root. Open **View > Full disk access** and follow the three steps: install the helper, allow it in System Settings > General > Login Items, and give dfdu Full Disk Access in System Settings > Privacy & Security. You only do this once. After that, a scan with the Full disk access switch on asks for an administrator's password. **View > Full disk access** also removes the helper.
- **Linux:** each scan asks for your password through pkexec, so pkexec needs to be installed. From the terminal, `dfdu-gui --admin /srv` does the same.

On Windows there is no switch: right-click dfdu and choose **Run as administrator**.

On macOS, an app opened from Finder does not get the Full Disk Access you may have given your terminal. Without it, folders such as Mail, Messages, Safari and Photos show as unreadable, and a banner offers to open the privacy settings.

## Themes

The app follows your system's light or dark setting. Pick a theme from the **View** menu or the command palette, or start with `--theme dark`, `--theme light` or `--theme NAME`. A [theme file](/docs/themes/) colours both the terminal browser and the app.

## Command line

```text
dfdu-gui [PATH] [--read-only] [--import FILE] [--no-rules] [--theme NAME] [--admin]
```

| Option | Meaning |
| --- | --- |
| `PATH` | The folder to scan. Without it, the start screen opens. |
| `--read-only` | Never delete anything. |
| `--import FILE` | Browse a saved scan: a dfdu snapshot or an ncdu export. |
| `--no-rules` | No categories and no cleanup suggestions. |
| `--theme NAME` | `auto`, `dark`, `light`, or a theme file's name. |
| `--admin` | Scan as an administrator (Linux). |

Remote scans over ssh and the command line tools (`scan`, `report`, `clean`, `diff`) are in `dfdu` only.

The app keeps its settings and window size in `gui.ron` in dfdu's state directory, next to the scan history.
