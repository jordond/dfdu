---
title: Install
description: Install dfdu with the install script, Homebrew, cargo binstall, Nix, the PowerShell installer, a release binary or from source.
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
```

| Option | Environment variable | Default |
| --- | --- | --- |
| `-b`, `--bin-dir DIR` | `DFDU_INSTALL_DIR` | `~/.local/bin` |
| `-v`, `--version VER` | `DFDU_VERSION` | the latest release |
| `--no-extras` | | man pages and completions are installed |
| | `DFDU_NO_VERIFY=1` | installs without a SHA-256 tool |

When `DIR` is not writable the binary is installed with `sudo`, and the man pages and completions are skipped. A directory in your home folder that you cannot write is refused instead. To read the script before running it, open [dfdu.app/install.sh](https://dfdu.app/install.sh) or [install.sh](https://github.com/jordond/dfdu/blob/main/install.sh) in the repository.

To uninstall, delete `~/.local/bin/dfdu`, `~/.local/share/man/man1/dfdu*.1`, `~/.local/share/bash-completion/completions/dfdu`, `~/.local/share/zsh/site-functions/_dfdu` and `~/.config/fish/completions/dfdu.fish`.

## Windows

Into `%USERPROFILE%\.cargo\bin`:

```powershell
powershell -ExecutionPolicy Bypass -c "irm https://github.com/jordond/dfdu/releases/latest/download/dfdu-installer.ps1 | iex"
```

## Package managers

```sh
brew install jordond/tap/dfdu                               # macOS, Linux
cargo binstall --git https://github.com/jordond/dfdu dfdu   # prebuilt, through cargo
nix run github:jordond/dfdu                                 # run from the flake
```

Homebrew installs the binary, the man page and the shell completions.

## Release binaries

Binaries for macOS (arm64, x86_64), Linux (static musl, arm64 and x86_64) and Windows (x86_64) are on the [releases page](https://github.com/jordond/dfdu/releases), with man pages and shell completions.

## From source

With Rust 1.95 or newer:

```sh
cargo install --locked --git https://github.com/jordond/dfdu dfdu
```
