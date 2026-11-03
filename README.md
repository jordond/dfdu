<picture>
  <source media="(prefers-color-scheme: dark)" srcset="assets/logo/dfdu-logo-dark.svg">
  <img width="320" src="assets/logo/dfdu-logo.svg" alt="dfdu">
</picture>

A disk usage browser for the terminal and the desktop. Browse, sort and delete, see what your space is made of (caches, build output, emulators, containers), and get a list of what is safe to remove.

![Browsing a home directory, the treemap and the cleanup view](assets/demos/browse.gif)

## Features

- **Fast.** Parallel scanning tuned to the storage device, and you can start browsing before it finishes.
- **Cleanup view.** Recognises caches, build folders, SDKs and old installers, says how safe each is to delete and how much it frees. [Rules](https://dfdu.app/docs/rules/) are extendable.
- **Treemap, tree mode, duplicates and largest files**, one key away from the list.
- **Opens last time's scan at once.** A repeat `dfdu PATH` shows the cached scan and refreshes only what changed.
- **History.** Every scan is recorded, so you can ask what grew since last week.
- **Remote.** Browse and delete on another machine over ssh.
- **Desktop app.** The same browser in a window for macOS, Linux and Windows, with the same keys as the terminal browser and a command palette.
- **Scriptable.** JSON and NDJSON output, a CI mode that fails above a size limit, and ncdu export import.
- **Safe by default.** Deletes go to the Trash; permanent deletes ask you to type `delete`. `--read-only` removes the delete keys.

## Install

```sh
curl -fsSL https://dfdu.app/install.sh | sh                 # macOS, Linux
brew install jordond/tap/dfdu                               # macOS, Linux
```

Windows:

```powershell
powershell -ExecutionPolicy Bypass -c "irm https://github.com/jordond/dfdu/releases/latest/download/dfdu-installer.ps1 | iex"
```

The desktop app:

```sh
brew install --cask jordond/tap/dfdu-gui                    # macOS
curl -fsSL https://dfdu.app/install.sh | sh -s -- --gui     # macOS, Linux x86_64, with the CLI
```

[The install docs](https://dfdu.app/docs/install/) cover release binaries, cargo, Nix, the install script's options, and the desktop app's downloads for Linux and Windows.

## Usage

```sh
dfdu ~                    # browse a directory
dfdu --disks              # pick a disk first
dfdu --remote server /srv # another host over ssh
```

Keys follow ncdu. Press `?` in the app for the full list, or see [Browsing](https://dfdu.app/docs/usage/).

```sh
dfdu report ~/dev         # what the cleanup rules found
dfdu clean ~/dev          # dry run of the recommended deletes
dfdu diff ~ --since 7d    # what grew in the last week
```

[Command line](https://dfdu.app/docs/cli/) has the commands for scripts and CI, with JSON output and size limits.

## Docs

- [Install](https://dfdu.app/docs/install/): every install method, options and uninstalling
- [Browsing](https://dfdu.app/docs/usage/): keys, views, mounts and the scan cache
- [Command line](https://dfdu.app/docs/cli/): the commands for scripts and CI
- [Desktop app](https://dfdu.app/docs/desktop/): installing and using the desktop app
- [Cleanup rules](https://dfdu.app/docs/rules/): the built-in rules and how to write your own
- [Themes](https://dfdu.app/docs/themes/): five built-in themes and custom TOML themes
- [Benchmarks](https://dfdu.app/docs/benchmarks/): scan time and memory against other tools
- [Contributing](CONTRIBUTING.md): building, testing and releasing

## Similar tools

Other disk usage tools worth a look:

- [ncdu](https://dev.yorhel.nl/ncdu): the classic ncurses disk usage browser, whose keys dfdu follows
- [gdu](https://github.com/dundee/gdu): a fast disk usage analyzer with a console interface, written in Go
- [dua](https://github.com/Byron/dua-cli): disk usage analyzer with an interactive mode, written in Rust
- [Dust](https://github.com/bootandy/dust): a more intuitive version of `du`, written in Rust
- [pdu](https://github.com/KSXGitHub/parallel-disk-usage): parallel disk usage, written in Rust

## License

[MIT](LICENSE)
