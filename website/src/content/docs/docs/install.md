---
title: Install
description: Install dfdu with the install script, Homebrew, the AUR, cargo binstall, Nix, the PowerShell installer, a release binary or from source. The desktop app has its own downloads.
---

## Install script

macOS and Linux:

```sh
curl -fsSL https://dfdu.app/install.sh | sh
```

The script downloads the release for your machine from GitHub, checks it against its published SHA-256 (it stops if the hash differs or no `sha256sum`, `shasum` or `openssl` is found), and installs:

- `dfdu` into `~/.local/bin`
- the man pages into `~/.local/share/man`
- completions for bash, zsh and fish into their per-user directories

It edits no shell profile. If `~/.local/bin` is not on your `PATH`, it prints the line to add. bash loads the completions through bash-completion 2 (macOS's own bash 3.2 has none: `brew install bash-completion@2`), fish at once, and zsh once `~/.local/share/zsh/site-functions` is on `fpath`:

```sh
fpath+=($HOME/.local/share/zsh/site-functions)   # in ~/.zshrc, before compinit
```

Run the script again to update. Options go after `sh -s --`:

```sh
curl -fsSL https://dfdu.app/install.sh | sh -s -- --bin-dir /usr/local/bin   # uses sudo if needed
curl -fsSL https://dfdu.app/install.sh | sh -s -- --version 0.1.0           # a specific release
curl -fsSL https://dfdu.app/install.sh | sh -s -- --no-extras               # the binary only
curl -fsSL https://dfdu.app/install.sh | sh -s -- --gui                     # the desktop app too
```

| Option | Environment variable | Default |
| --- | --- | --- |
| `-b`, `--bin-dir DIR` | `DFDU_INSTALL_DIR` | `~/.local/bin` |
| `-v`, `--version VER` | `DFDU_VERSION` | the latest release |
| `--no-extras` | | man pages and completions are installed |
| `--gui` | `DFDU_GUI=1` | the desktop app is not installed |
| `--app-dir DIR` | `DFDU_APP_DIR` | `~/Applications` (macOS) |
| | `DFDU_NO_VERIFY=1` | installs without a SHA-256 tool |

When `DIR` is not writable the binary is installed with `sudo`, and the man pages and completions are skipped. A directory in your home folder that you cannot write is refused instead. To read the script before running it, open [dfdu.app/install.sh](https://dfdu.app/install.sh) or [install.sh](https://github.com/jordond/dfdu/blob/main/install.sh) in the repository.

`--gui` installs the desktop app from the same release once the CLI is in place, checked against its SHA-256 the same way. The GUI ships from 0.3.0, and for aarch64 Linux from 0.5.1: with an earlier `--version` the CLI is installed and the script then exits with an error.

- macOS: `dfdu.app` into `~/Applications`, or the `--app-dir` directory (with `sudo` if it is not writable, as for `--bin-dir`). An existing `dfdu.app` is replaced. To scan every folder as an administrator, open View > Full disk access in dfdu. It installs a helper that macOS asks you to allow in System Settings > General > Login Items.
- Linux (x86_64 and aarch64): the AppImage as `dfdu-gui` beside `dfdu`, and, unless `--no-extras`, a desktop entry in `~/.local/share/applications/app.dfdu.desktop` with its icon under `~/.local/share/icons/hicolor`. AppImages need FUSE 2; if `libfuse.so.2` is missing the script says so: install `libfuse2` (`libfuse2t64` on Ubuntu 24.04), `fuse2` or `fuse-libs`, or run `dfdu-gui --appimage-extract-and-run`.

To uninstall, delete `~/.local/bin/dfdu`, `~/.local/share/man/man1/dfdu*.1`, `~/.local/share/bash-completion/completions/dfdu`, `~/.local/share/zsh/site-functions/_dfdu` and `~/.config/fish/completions/dfdu.fish`, and for the GUI `~/Applications/dfdu.app` or `~/.local/bin/dfdu-gui`, `~/.local/share/applications/app.dfdu.desktop` and `~/.local/share/icons/hicolor/512x512/apps/app.dfdu.png`.

## Windows

Into `%USERPROFILE%\.cargo\bin`:

```powershell
powershell -ExecutionPolicy Bypass -c "irm https://github.com/jordond/dfdu/releases/latest/download/dfdu-installer.ps1 | iex"
```

## Package managers

```sh
brew install jordond/tap/dfdu                               # macOS, Linux
yay -S dfdu-bin                                             # Arch Linux, from the AUR
cargo binstall --git https://github.com/jordond/dfdu dfdu   # prebuilt, through cargo
nix run github:jordond/dfdu                                 # run from the flake
```

Homebrew and the AUR packages install the binary, the man pages and the shell completions. The AUR has three, for x86_64 and aarch64, and any AUR helper (`paru`) or `makepkg` installs them:

| Package | Installs |
| --- | --- |
| `dfdu-bin` | the release binary |
| `dfdu` | the release, built from source with cargo |
| `dfdu-git` | the latest commit on `main`, built from source |

## Release binaries

Binaries for macOS (arm64, x86_64), Linux (static musl, arm64 and x86_64) and Windows (x86_64) are on the [releases page](https://github.com/jordond/dfdu/releases), with man pages and shell completions.

## With an AI assistant

[dfdu.app/llms.txt](https://dfdu.app/llms.txt) describes dfdu and how to install it for coding assistants and other LLM tools. Paste this prompt into one:

```text
Install dfdu for me. Follow the instructions at https://dfdu.app/llms.txt and pick the method that fits this machine.
```

The full docs as one file are at [llms-full.txt](https://dfdu.app/llms-full.txt).

## Updating

```sh
dfdu update            # install the latest release over this dfdu
dfdu update --check    # only say whether there is a newer one
```

`dfdu update` downloads the release for your machine from GitHub, checks it against its published SHA-256 and replaces the running binary, along with any man pages and completions the install script put in place. A dfdu installed with Homebrew, Nix, Scoop, winget or a system package manager is left to that manager, and the command prints how to update it (`brew upgrade dfdu`). If the binary's directory is not writable, run `sudo dfdu update`. `--force` reinstalls the latest release even when you already have it.

The desktop app checks for a new release when it starts and once a day after that, and shows a notice when one is out. View > Updates and command-line tool… checks again and installs the update:

- macOS: `dfdu.app` is replaced where it is, then restarted. An app installed with the Homebrew cask is updated with `brew upgrade --cask dfdu-gui` instead.
- Linux: the AppImage is replaced. The .deb is updated with your package manager.
- Windows: the installer is downloaded and started, and the app closes for it. A Scoop install is updated with `scoop update dfdu-gui`, the zip by hand.

Turn the check off in that window, or set `DFDU_NO_UPDATE_CHECK=1`. The same window installs the command-line tool of the app's version: `dfdu` goes into `~/.local/bin` with its man pages and completions, or on Windows into `%LOCALAPPDATA%\Programs\dfdu`, which is added to your `PATH`.

## From source

With Rust 1.95 or newer, from crates.io:

```sh
cargo install --locked dfdu
```

Or the latest commit on `main`:

```sh
cargo install --locked --git https://github.com/jordond/dfdu dfdu
```

## Desktop app

From 0.3.0, the desktop app is a separate download:

```sh
brew install --cask jordond/tap/dfdu-gui                       # macOS
curl -fsSL https://dfdu.app/install.sh | sh -s -- --gui        # macOS, Linux, with the CLI
```

The [latest release](https://github.com/jordond/dfdu/releases/latest) has a disk image for macOS, a `.deb` and an AppImage for Linux (x86_64 and aarch64), and an installer and a portable zip for Windows. [Desktop app](/docs/desktop/#install) covers each one.
