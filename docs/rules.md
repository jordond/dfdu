# Cleanup rules

Rules are how dfdu recognises what it finds: a Rust `target/` folder, the npm cache, an Android emulator, an old installer in Downloads. Each rule gives what it matches a category, a safety level and a way to clean it up. The results show up in the browser's cleanup view (`3`), in `dfdu report` and in `dfdu clean`.

dfdu comes with a set of [built-in rules](#built-in-rules), and you can [write your own](#writing-rules) to add new ones or change the built-in ones.

## Safety levels

Every match has a safety level that says how much you lose by deleting it.

| Level         | Meaning                                                                    |
| ------------- | -------------------------------------------------------------------------- |
| `safe`        | A pure cache. It is rebuilt automatically and you will barely notice.      |
| `regenerable` | Rebuilt when needed, at a real cost: a download, a recompile, a reinstall. |
| `review`      | May hold your own work or data. Look before you delete.                    |
| `manual`      | Only reported, or cleaned up with the owning tool's own command.           |
| `protected`   | Never offered for deletion.                                                |

Operating system folders, cloud-synced folders such as iCloud Drive, credentials and tool configuration (`~/.cargo/bin`, `~/.docker/config.json`) and other important locations are protected no matter what any rule says. Rule files cannot change that.

Some rules lower a match's safety when something looks off. For example, a `node_modules` folder with no lockfile next to it becomes `review`, because reinstalling may give you different package versions.

## Cleanup actions

| Action   | What happens                                                                                     |
| -------- | ------------------------------------------------------------------------------------------------ |
| `delete` | The match is deleted.                                                                            |
| `trash`  | The match is moved to the Trash (Recycle Bin on Windows).                                        |
| `native` | dfdu runs the owning tool's own cleanup command, such as `cargo clean` or `docker system prune`. |
| `report` | Shown with its size only. dfdu does not clean it up.                                             |

`dfdu clean` only runs native commands when you pass `--allow-commands`.

## Recommendations

A match is _recommended_ for cleanup when it is big enough and old enough to be worth it. Each rule sets its own thresholds; a Cargo `target/` folder, for example, is recommended once it is over 100 MiB and has not been touched for 14 days. Smaller or newer matches are still listed, just not recommended.

## Built-in rules

The **Default** column says where a rule is on out of the box. A rule is on only on the platforms where its paths have been checked against a real machine, and stays off elsewhere until they have been; you can [turn them on yourself](#turning-rules-on-and-off). Rules marked _Off_ are off everywhere.

### Project build output and dependencies

| ID                     | Matches                                                                                           | Safety      | Cleanup       | Default |
| ---------------------- | ------------------------------------------------------------------------------------------------- | ----------- | ------------- | ------- |
| `generic.cachedir-tag` | Any folder with a valid [`CACHEDIR.TAG`](https://bford.info/cachedir/), which marks it as a cache | safe        | delete        | All     |
| `rust.target`          | `target/` next to a `Cargo.toml`                                                                  | regenerable | `cargo clean` | All     |
| `node.node_modules`    | `node_modules/` next to a `package.json`                                                          | regenerable | delete        | All     |
| `python.venv`          | `.venv/` or `venv/` containing `pyvenv.cfg`                                                       | regenerable | delete        | All     |

### Package manager caches

| ID                   | Matches                                     | Safety      | Cleanup                    | Default |
| -------------------- | ------------------------------------------- | ----------- | -------------------------- | ------- |
| `npm.cache`          | npm cache                                   | safe        | `npm cache clean --force`  | macOS, Linux |
| `pnpm.store`         | pnpm store                                  | regenerable | `pnpm store prune`         | macOS   |
| `yarn.cache`         | Yarn cache                                  | safe        | delete                     | macOS, Linux |
| `bun.cache`          | Bun install cache                           | safe        | delete                     | macOS, Linux |
| `pip.cache`          | pip cache                                   | safe        | delete                     | macOS, Linux |
| `uv.cache`           | uv cache                                    | safe        | `uv cache clean`           | macOS, Linux |
| `cargo.registry`     | Cargo registry and git caches               | regenerable | delete                     | macOS, Linux |
| `go.build-cache`     | Go build cache                              | safe        | `go clean -cache`          | macOS, Linux |
| `go.module-cache`    | Go module cache                             | regenerable | `go clean -modcache`       | macOS, Linux |
| `gradle.user-caches` | Gradle caches and wrapper distributions     | regenerable | delete                     | macOS, Linux |
| `maven.repository`   | Maven local repository (`~/.m2/repository`) | regenerable | delete                     | macOS, Linux |
| `homebrew.cache`     | Homebrew download cache                     | safe        | `brew cleanup --prune=all` | macOS, Linux |
| `cocoapods.cache`    | CocoaPods cache (macOS only)                | safe        | delete                     | macOS   |

### Xcode and Apple simulators

All macOS only.

| ID                        | Matches                                         | Safety      | Cleanup                           | Default |
| ------------------------- | ----------------------------------------------- | ----------- | --------------------------------- | ------- |
| `xcode.derived-data`      | Each project's folder in DerivedData            | safe        | delete                            | macOS   |
| `xcode.device-support`    | Device support files, one folder per OS version | regenerable | delete                            | macOS   |
| `xcode.archives`          | `.xcarchive` app archives                       | review      | trash                             | macOS   |
| `xcode.simulator-devices` | Simulator devices and their data                | review      | `xcrun simctl delete unavailable` | macOS   |
| `xcode.caches`            | Xcode and CoreSimulator caches                  | safe        | delete                            | macOS   |

### Android

| ID                      | Matches                                          | Safety      | Cleanup                 | Default |
| ----------------------- | ------------------------------------------------ | ----------- | ----------------------- | ------- |
| `android.avd`           | Android emulators (`*.avd`)                      | review      | `avdmanager delete avd` | macOS, Linux |
| `android.system-images` | Emulator system images, one folder per API level | regenerable | delete                  | macOS, Linux |

### Containers

| ID                    | Matches                                         | Safety | Cleanup               | Default |
| --------------------- | ----------------------------------------------- | ------ | --------------------- | ------- |
| `docker.desktop-disk` | Docker Desktop's disk image (macOS and Windows) | manual | `docker system prune` | macOS   |
| `orbstack.data`       | OrbStack data (macOS)                           | manual | report                | Off     |
| `podman.storage`      | Rootless Podman storage (Linux)                 | manual | `podman system prune` | Off     |

### Toolchains and editors

| ID                  | Matches                                                         | Safety      | Cleanup                      | Default |
| ------------------- | --------------------------------------------------------------- | ----------- | ---------------------------- | ------- |
| `rustup.toolchains` | Rust toolchains installed by rustup                             | regenerable | `rustup toolchain uninstall` | macOS, Linux |
| `jetbrains.caches`  | JetBrains IDE and Android Studio caches, one folder per version | regenerable | delete                       | macOS   |
| `vscode.caches`     | VS Code caches                                                  | safe        | delete                       | macOS   |

### System and apps

| ID                     | Matches                                                           | Safety | Cleanup | Default |
| ---------------------- | ----------------------------------------------------------------- | ------ | ------- | ------- |
| `sys.user-caches`      | Your user cache folder (`~/Library/Caches`, `~/.cache`, `%TEMP%`) | safe   | delete  | macOS, Linux |
| `sys.logs`             | `~/Library/Logs` (macOS)                                          | safe   | delete  | macOS   |
| `sys.crash-reports`    | Crash and diagnostic reports (macOS)                              | safe   | delete  | macOS   |
| `sys.trash`            | The Trash (macOS and Linux)                                       | review | delete  | macOS   |
| `browser.chrome-cache` | Google Chrome's cache                                             | safe   | delete  | macOS   |

### Your files

These look through your home folder, leaving out hidden folders and `~/Library` (or `AppData` on Windows).

| ID                             | Matches                                                                                                 | Safety | Cleanup | Default |
| ------------------------------ | ------------------------------------------------------------------------------------------------------- | ------ | ------- | ------- |
| `content.installers-downloads` | Installers and disk images (`.dmg`, `.pkg`, `.iso`, `.msi`, `.deb`, ...) in Downloads or on the Desktop | review | trash   | macOS, Linux |
| `content.extracted-archives`   | Archives in Downloads or on the Desktop with a folder of the same name next to them                     | review | trash   | macOS, Linux |
| `content.large-videos`         | Video files over 1 GiB                                                                                  | review | trash   | macOS, Linux |
| `content.large-old`            | Files over 1 GiB not modified in 180 days                                                               | review | trash   | macOS, Linux |
| `content.empty-dirs`           | Empty folders                                                                                           | review | trash   | Off     |
| `content.broken-links`         | Symbolic links whose target no longer exists                                                            | review | delete  | macOS, Linux |

The full definitions, with every path and threshold, are in [`crates/dfdu-rules/src/builtin.toml`](../crates/dfdu-rules/src/builtin.toml). They use the same format as your own rule files, so they are also a good source of examples.

## Writing rules

Rule files are TOML files in the `dfdu/rules` folder of your configuration directory:

| Platform     | Folder                                                      |
| ------------ | ----------------------------------------------------------- |
| macOS, Linux | `~/.config/dfdu/rules/` (or `$XDG_CONFIG_HOME/dfdu/rules/`) |
| Windows      | `%APPDATA%\dfdu\rules\`                                     |

Every `*.toml` file in the folder is loaded, in name order, after the built-in rules. Each file starts with `schema_version = 1` and holds one or more `[[rule]]` entries.

If a rule has a mistake, dfdu prints the file, the rule's ID and the problem when it starts, skips that rule and loads the rest. Misspelt keys are reported too, never silently ignored.

### A first rule

Say you render animations into `~/renders` and want dfdu to flag old render folders:

```toml
schema_version = 1

[[rule]]
id = "me.renders"
category = "content.media"
name = "Render output"
safety = "regenerable"
action = "trash"
explanation = "Frames rendered by Blender."
lost = "The frames; render them again."
min_size = "1 GiB"
min_age_days = 30

[rule.match]
kind = "dir"
paths = ["{home}/renders/*"]
stop_descent = true
```

Save it as `~/.config/dfdu/rules/renders.toml`, then check what it finds:

```sh
dfdu report ~/renders
```

`report` lists every match with its safety level, size, age and rule name, so it is the quickest way to try a rule out. Nothing is deleted. Add `--format json` to see the rule ID and every other detail.

### Turning rules on and off

An entry with only `id` and `enabled` switches an existing rule on or off:

```toml
schema_version = 1

# Don't flag node_modules folders.
[[rule]]
id = "node.node_modules"
enabled = false

# Turn on a rule that is off on this platform by default.
[[rule]]
id = "npm.cache"
enabled = true
```

To turn off every rule for one run, pass `--no-rules`.

### Replacing a built-in rule

A rule with the same `id` as an earlier one replaces it completely. Copy the rule from [`builtin.toml`](../crates/dfdu-rules/src/builtin.toml) into your own file and change what you need.

### Matching

The `[rule.match]` table decides which entries a rule matches. It needs `names`, `paths` or both; with both, an entry has to match one of each.

```toml
[rule.match]
kind = "dir"                          # dir, file or any (default any)
names = ["target"]                    # the entry's own name
paths = ["{home}/projects/**"]        # the entry's full path
exclude = ["{home}/projects/keep/**"] # paths that never match
stop_descent = true
```

| Key            | Meaning                                                                                                                                            |
| -------------- | -------------------------------------------------------------------------------------------------------------------------------------------------- |
| `kind`         | `dir`, `file` or `any` (the default).                                                                                                              |
| `names`        | Patterns for the entry's name. No `/` allowed.                                                                                                     |
| `paths`        | Patterns for the entry's full path.                                                                                                                |
| `exclude`      | Paths that are never matched, even when the rest matches.                                                                                          |
| `stop_descent` | Don't apply other rules to anything inside a match. Use it for caches and build folders, where what is inside belongs to the match.                |
| `top_most`     | Match only the outermost entry, not ones nested inside another match of the same rule (a `node_modules` inside a `node_modules`).                  |
| `sibling`      | Something that must exist next to the entry: `[[rule.match.sibling]]` with `any_of = [...]`. Each `sibling` table needs at least one of its names. |
| `child`        | Something that must exist inside the entry: `[[rule.match.child]]` with `any_of = [...]`.                                                          |
| `marker`       | A file inside the entry, optionally starting with given text: `[[rule.match.marker]]` with `file`, `prefix` and `required = true`.                 |

Files have a few more conditions:

| Key                 | Meaning                                                                                      |
| ------------------- | -------------------------------------------------------------------------------------------- |
| `larger_than`       | Only files using more disk space than this (`"1 GiB"`). Needs `kind = "file"`.               |
| `not_modified_days` | Only files not modified for this many days. Needs `kind = "file"`.                           |
| `broken_link`       | Only symbolic links whose target does not exist. Needs `kind = "file"`.                      |
| `empty`             | Only folders with nothing in them (a `.DS_Store` counts as something). Needs `kind = "dir"`. |

`larger_than` and `not_modified_days` decide whether a file matches at all. `min_size` and `min_age_days` on the rule itself only decide whether a match is [recommended](#recommendations).

For example, a `target/` folder that sits next to a `Cargo.toml` and has build output inside it:

```toml
[rule.match]
kind = "dir"
names = ["target"]
stop_descent = true
top_most = true
  [[rule.match.sibling]]
  any_of = ["Cargo.toml"]
  [[rule.match.child]]
  any_of = ["CACHEDIR.TAG", "debug", "release"]
```

### Patterns

| Pattern | Matches                                   |
| ------- | ----------------------------------------- |
| `*`     | Anything within one path component        |
| `**`    | Anything across components, including `/` |
| `?`     | Any one character except `/`              |

`paths` patterns are anchored: they match the whole path, so start them with a variable such as `{home}` or an absolute path. Matching ignores case on macOS and Windows and is case-sensitive on Linux.

### Variables

| Variable          | macOS                           | Linux                                | Windows                                    |
| ----------------- | ------------------------------- | ------------------------------------ | ------------------------------------------ |
| `{home}`          | Your home folder                | Your home folder                     | Your home folder                           |
| `{cache}`         | `~/Library/Caches`              | `$XDG_CACHE_HOME` or `~/.cache`      | `%LOCALAPPDATA%`                           |
| `{config}`        | `~/Library/Application Support` | `$XDG_CONFIG_HOME` or `~/.config`    | `%APPDATA%`                                |
| `{data}`          | `~/Library/Application Support` | `$XDG_DATA_HOME` or `~/.local/share` | `%APPDATA%`                                |
| `{local_data}`    | `~/Library/Application Support` | `$XDG_DATA_HOME` or `~/.local/share` | `%LOCALAPPDATA%`                           |
| `{temp}`          | `$TMPDIR`                       | `$TMPDIR` or `/tmp`                  | `%TEMP%`                                   |
| `{downloads}`     | `~/Downloads`                   | `~/Downloads`                        | `~/Downloads`                              |
| `{program_files}` | `/Applications`                 | `/opt` and `/usr`                    | `%ProgramFiles%` and `%ProgramFiles(x86)%` |

`{env:NAME|fallback}` uses the environment variable `NAME`, or the fallback when it is not set. The fallback can contain variables itself:

```toml
paths = ["{env:CARGO_HOME|{home}/.cargo}/registry/cache"]
```

In `sibling` and `child` names you can also use `{name}`, the matched entry's name, and `{name_stem}`, its name without the extension (`src.tar.gz` gives `src`). This is how the built-in rule finds archives that were already extracted:

```toml
[[rule.match.sibling]]
any_of = ["{name_stem}"]
```

### Different paths per platform

Any list in `[rule.match]` can be given per platform instead:

```toml
paths = { macos = ["{cache}/Yarn"], linux = ["{cache}/yarn"], windows = ["{local_data}/Yarn/Cache"] }
```

To limit a whole rule to some platforms, set `platforms = ["macos", "linux"]` on the rule.

### Safety checks

The `[rule.guard]` table adds checks made before anything is deleted:

| Key               | Meaning                                                                                                                    |
| ----------------- | -------------------------------------------------------------------------------------------------------------------------- |
| `not_running`     | Process names. The match is not cleaned up while one of them runs (`["cargo", "rustc"]`).                                  |
| `not_vcs_tracked` | When `true`, a match that version control tracks files in is protected. A `target/` folder committed to git is left alone. |
| `not_inside`      | Paths the match must not be inside (`["**/*.app"]`).                                                                       |

The `[rule.degrade]` table lowers safety when something is missing next to the match. A condition can only move a match towards `review`, never make it safer:

```toml
[rule.degrade]
  [[rule.degrade.when]]
  missing_sibling = ["package-lock.json", "yarn.lock", "pnpm-lock.yaml"]
  safety = "review"
  reason = "No lockfile next to it: reinstalling may resolve different versions."
```

### Native cleanup commands

With `action = "native"`, dfdu runs the owning tool's own cleanup instead of deleting files. That is usually the better choice when a tool keeps an index of its files (Docker, pnpm) or makes them read-only (Go's module cache).

```toml
action = "native"

[rule.native]
check = ["cargo", "--version"]         # run first, to see that the tool is installed
dry_run = ["cargo", "clean", "--dry-run"]
command = ["cargo", "clean"]
cwd = "{parent}"
```

| Key            | Meaning                                                                                         |
| -------------- | ----------------------------------------------------------------------------------------------- |
| `command`      | The cleanup command. Required.                                                                  |
| `check`        | Run first to see that the tool is there.                                                        |
| `dry_run`      | Shows what would be removed. Runs before you confirm.                                           |
| `pre`          | Commands to run before `command`, such as stopping a daemon: `[["gradle", "--stop"]]`.          |
| `cwd`          | The folder to run in.                                                                           |
| `size_command` | Reports the tool's own figure for how much space cleanup would free, shown in the cleanup view. |

Commands are lists of arguments and are run directly, never through a shell. They can use `{path}`, `{parent}`, `{name}` and `{name_stem}` of the matched entry.

For cleanups that go further, add `[[rule.native.variant]]` entries with a `name`, a `command` and an optional `dry_run`. Each is offered separately and has to be confirmed twice:

```toml
[[rule.native.variant]]
name = "also every image no container uses"
command = ["docker", "system", "prune", "--all", "--force"]
```

### Rule reference

| Key              | Required      | Meaning                                                                                                                    |
| ---------------- | ------------- | -------------------------------------------------------------------------------------------------------------------------- |
| `id`             | yes           | A unique dotted name, such as `me.renders`. Use your own prefix so it does not clash with built-in IDs.                    |
| `category`       | yes           | One of the [categories](#categories).                                                                                      |
| `name`           | yes           | A short name shown in the cleanup view and reports.                                                                        |
| `safety`         | yes           | `safe`, `regenerable`, `review` or `manual`.                                                                               |
| `action`         |               | `delete` (the default), `trash`, `native` or `report`.                                                                     |
| `explanation`    |               | What the match is.                                                                                                         |
| `lost`           |               | What you lose by deleting it.                                                                                              |
| `min_size`       |               | Recommended only when at least this big (`"500 MiB"`).                                                                     |
| `min_age_days`   |               | Recommended only when not changed for this many days.                                                                      |
| `priority`       |               | When several rules match the same entry, the highest priority wins (default 50). Ties go to the rule with more conditions. |
| `platforms`      |               | Platforms the rule applies to: `macos`, `linux`, `windows`. Default all.                                                   |
| `enabled`        |               | `false` to define the rule but leave it off.                                                                               |
| `[rule.match]`   | yes           | See [Matching](#matching).                                                                                                 |
| `[rule.guard]`   |               | See [Safety checks](#safety-checks).                                                                                       |
| `[rule.degrade]` |               | See [Safety checks](#safety-checks).                                                                                       |
| `[rule.native]`  | with `native` | See [Native cleanup commands](#native-cleanup-commands). Only for `action = "native"`.                                     |

### Categories

| ID                     | Name                           |
| ---------------------- | ------------------------------ |
| `dev.build`            | Project build output           |
| `dev.deps`             | Project dependencies           |
| `dev.pkgcache`         | Package manager caches         |
| `dev.xcode`            | Xcode and Apple simulators     |
| `dev.android`          | Android SDK and emulators      |
| `dev.toolchains`       | Toolchains and runtimes        |
| `dev.containers`       | Containers and VMs             |
| `dev.ide`              | IDE and editor caches          |
| `dev.nix`              | Nix                            |
| `sys.cache`            | OS and user caches             |
| `sys.logs`             | Logs                           |
| `sys.crash`            | Crash reports and dumps        |
| `sys.trash`            | Trash                          |
| `sys.updates`          | OS update leftovers            |
| `sys.snapshots`        | Snapshots                      |
| `app.browser`          | Browser caches                 |
| `app.chat`             | Chat and meeting apps          |
| `app.media`            | Media apps                     |
| `app.games`            | Games                          |
| `app.mail`             | Mail                           |
| `app.mobile`           | Mobile device data             |
| `content.large-old`    | Large, not recently used files |
| `content.duplicates`   | Duplicate files                |
| `content.installers`   | Installers and disk images     |
| `content.media`        | Large video files              |
| `content.extracted`    | Archives already extracted     |
| `content.empty`        | Empty files and folders        |
| `content.broken-links` | Broken symbolic links          |

`dfdu report --category dev` and `dfdu clean --category dev` filter by a category or by its first part.
