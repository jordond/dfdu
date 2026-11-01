# dfdu-gui

The dfdu browser in a desktop window. It runs on the same session driver as the terminal
browser (`dfdu-session`), so scans, marks, cleanup rules, delete gates, undo and the scan history
behave as they do in `dfdu`. Drawing uses egui/eframe 0.37 with wgpu, falling back to OpenGL
when wgpu cannot start.

## Building locally

`dfdu-gui` is a workspace member but not in `default-members`, so a plain `cargo build` or
`cargo test` at the root never compiles it. Name it with `-p`:

```sh
cargo run -p dfdu-gui                       # start screen with the disks
cargo run -p dfdu-gui -- ~/Downloads        # scan a folder
cargo build -p dfdu-gui --release           # target/release/dfdu-gui
```

The toolchain is the one in `rust-toolchain.toml` (stable, at least the workspace's
`rust-version`).

### Platform requirements

- **macOS:** nothing extra.
- **Windows:** nothing extra (MSVC toolchain).
- **Linux:** the windowing and GL development libraries. On Debian/Ubuntu:

  ```sh
  sudo apt-get install libxkbcommon-dev libwayland-dev libx11-dev \
    libxcursor-dev libxrandr-dev libxi-dev libgl1-mesa-dev
  ```

## Command line

```text
dfdu-gui [PATH] [--read-only] [--import FILE] [--no-rules] [--theme NAME]
```

| Flag | Meaning |
|---|---|
| `PATH` | Directory to scan. Without it, the start screen lists the disks. |
| `--read-only` | Never delete anything; every delete path is hidden. |
| `--import FILE` | Browse a saved scan (dfdu snapshot, ncdu JSON plain or zstd, ncdu binary), read-only. |
| `--no-rules` | No categories and no cleanup candidates. |
| `--theme NAME` | `auto`, `dark`, `light`, or a theme file `NAME.toml` in dfdu's themes directory. |

Remote scans (`--remote`) and root-only features (`--mft`, exact btrfs sizes) are CLI only.

## Using it

The views are List, Map, Cleanup, Duplicates and Largest. The keys are the terminal browser's;
`?` shows them, and Cmd-K (Ctrl-K elsewhere) opens the command palette. Folders can be dropped
on the window to scan them.

On macOS, an app does not inherit the terminal's Full Disk Access, so folders like
`~/Library/Mail` read as permission errors. A banner says so and opens the settings pane.

Settings (theme, how the list is shown, window size) are kept in `gui.ron` in dfdu's state
directory, beside the scan history. `DFDU_STATE_DIR` moves it, which is handy for a clean run:

```sh
DFDU_STATE_DIR=$(mktemp -d) cargo run -p dfdu-gui
```

## Tests and lints

```sh
cargo test -p dfdu-gui
cargo clippy -p dfdu-gui --all-targets -- -D warnings
```

The tests drive the window headless with `egui_kittest` (`src/tests/`): flows such as
scan-browse-mark-stage-undo, the delete gates, read-only and imported trees, and a walk that
fails on any control without an accessible name. CI runs both commands on macOS, Windows and
Ubuntu, and checks that `dfdu` itself pulls in no GUI crate:

```sh
cargo tree -p dfdu -e normal | grep -E '^(egui|eframe|wgpu|winit) '   # must print nothing
```

### Screenshots

A hidden switch takes a picture of the window and exits, for checking by eye or in CI:

```sh
DFDU_GUI_SCREENSHOT=shot.ppm cargo run -p dfdu-gui -- ~/Downloads
DFDU_GUI_SCREENSHOT=shot.ppm DFDU_GUI_SCREENSHOT_KEYS='i' cargo run -p dfdu-gui -- ~/Downloads
```

The picture is taken a second after the scan finishes, as a binary PPM. Keys in
`DFDU_GUI_SCREENSHOT_KEYS` are typed first, one every 400 ms (a space is Space, a newline is
Enter).

## Releasing

The GUI has its own version in `Cargo.toml` and is left out of the CLI's `dist` release
(`dist = false`). It will ship on `gui-v*` tags with its own installers (dmg, NSIS, AppImage,
deb) and a Homebrew cask; that workflow is not written yet (Phase 7 in `GUI-PLAN.md`).
