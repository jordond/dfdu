# Rules

Rules label what the scan finds (a Rust `target/`, the npm cache) with a category, a safety level and a way to clean it up. The code is `crates/dfdu-rules`, and the built-in pack is `crates/dfdu-rules/src/builtin.toml`.

## Writing rules

User rules go in `$CONFIG/dfdu/rules/*.toml` (`~/.config/dfdu/rules` on macOS and Linux, `%APPDATA%\dfdu\rules` on Windows), loaded in name order after the built-in pack. A rule that does not load is reported when dfdu starts and left out. `--no-rules` turns classification off.

```toml
schema_version = 1

# Turn a built-in rule off (or on, for one not yet checked on this platform).
[[rule]]
id = "node.node_modules"
enabled = false

# A new rule, or a replacement for a built-in one with the same id.
[[rule]]
id = "me.renders"
category = "content.media"          # a built-in category
name = "Render output"
safety = "regenerable"              # safe | regenerable | review | manual
action = "trash"                    # delete | trash | native | report (default delete)
explanation = "Frames rendered by Blender."
lost = "The frames; render them again."
min_size = "1 GiB"
[rule.match]
kind = "dir"                        # dir | file | any (default any)
paths = ["{home}/renders/*"]       # anchored globs; * within a component, ** across them
stop_descent = true
```

- A rule needs `names` (basename globs) or `paths` (anchored path globs), or both, in which case both must match. `exclude` rejects a match, `sibling` needs a name next to it, `child` and a required `marker` need something inside it.
- Variables: `{home}`, `{cache}`, `{config}`, `{data}`, `{local_data}`, `{temp}`, `{downloads}`, `{program_files}`, `{env:NAME|fallback}`; in sibling and child names also `{name}` and `{name_stem}` (the name without its last extension, and without a `.tar` before it: `src.tar.gz` gives `src`). Any list may be a per-platform table: `paths = { macos = [...], linux = [...] }`.
- For `kind = "file"` only, `larger_than = "1 GiB"` (disk usage) and `not_modified_days = 180` (modification time, since access times are often not kept) are checked when the file is classified, so they decide whether it matches at all. `min_size` and `min_age_days` only decide whether a match is recommended.
- `broken_link = true` (`kind = "file"`) matches a symbolic link whose target does not exist; the link is followed once its path matched. `empty = true` (`kind = "dir"`) matches a directory with no entries at all (a `.DS_Store` counts), decided when its listing ends.
- Unknown keys, categories, safety levels and variables are errors. A rule with an error is left out and reported with its file and id; the other rules still load.
- `degrade` conditions may only lower safety (move it towards `review`), never raise it.
- `action = "native"` needs `[rule.native]` with `command`, the owning tool's own cleanup (argv, no shell). `check` is run first to see that the tool is there, `dry_run` shows what would go and runs before the user confirms, `pre` runs before the command (stopping a daemon), `cwd` sets where it runs, `size_command` reports the tool's own count for the cleanup view (docker's JSON lines are parsed). Commands may use `{path}`, `{parent}`, `{name}` and `{name_stem}` of the matched node. `[[rule.native.variant]]` entries (`name`, `command`, optional `dry_run`) are further-reaching forms, offered separately and confirmed twice.
- Protected paths (`crates/dfdu-rules/src/protected.rs`) win over every rule and cannot be changed by rule files.
