---
title: Install
description: Install dfdu with Homebrew, cargo binstall, Nix, the shell or PowerShell installer, a release binary or from source.
---

## Package managers

```sh
brew install jordond/tap/dfdu                               # macOS, Linux
cargo binstall --git https://github.com/jordond/dfdu dfdu   # prebuilt, through cargo
nix run github:jordond/dfdu                                 # run from the flake
```

Homebrew installs the binary, the man page and the shell completions.

## Installers

macOS and Linux, into `~/.cargo/bin`:

```sh
curl -LsSf https://github.com/jordond/dfdu/releases/latest/download/dfdu-installer.sh | sh
```

Windows, into `%USERPROFILE%\.cargo\bin`:

```powershell
powershell -ExecutionPolicy Bypass -c "irm https://github.com/jordond/dfdu/releases/latest/download/dfdu-installer.ps1 | iex"
```

## Release binaries

Binaries for macOS (arm64, x86_64), Linux (static musl, arm64 and x86_64) and Windows (x86_64) are on the [releases page](https://github.com/jordond/dfdu/releases), with man pages and shell completions.

## From source

With Rust 1.95 or newer:

```sh
cargo install --locked --git https://github.com/jordond/dfdu dfdu
```
