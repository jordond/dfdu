# AGENTS.md

Instructions for coding agents working in this repository. [CONTRIBUTING.md](CONTRIBUTING.md) has the commands and a map of the crates, and the [architecture page](website/src/content/docs/docs/architecture.md) explains how a scan flows through them.

## Commits: Conventional Commits

Every commit, and every pull request title (pull requests are squash-merged with the title as the commit subject, and `.github/workflows/pr-title.yml` checks it), follows [Conventional Commits](https://www.conventionalcommits.org/en/v1.0.0/). The changelog and the next version are generated from them (`cliff.toml`, `cargo xtask release bump`), so a wrong type ends up as a wrong release.

```
<type>(<scope>): <summary>

<body: why, not what>

BREAKING CHANGE: <what breaks and what to do instead>
```

| Type | Use for | Changelog | Version (before 1.0) |
|---|---|---|---|
| `feat` | Something new a user can see or use | Added | patch |
| `fix` | A bug a user could hit | Fixed | patch |
| `perf` | Faster or smaller, behaviour unchanged | Performance | patch |
| `revert` | Undoing an earlier commit | Removed | patch |
| `refactor` | Internal restructuring, no behaviour change | left out | none |
| `test` | Tests only | left out | none |
| `docs` | Docs, README, the website's docs pages | left out | none |
| `build` | Cargo manifests, dist, packaging scripts | left out | none |
| `ci` | `.github/workflows`, CI scripts | left out | none |
| `chore` | Dependencies, formatting, anything else | left out | none |
| `style` | Formatting only | left out | none |

- **Breaking changes**: put `!` after the type or scope (`feat(cli)!: ...`) and add a `BREAKING CHANGE:` footer. Before 1.0 this bumps the minor version; after 1.0, the major. A change to the command line, the JSON or NDJSON output, the rule file schema, the theme format, or the snapshot and cache formats a newer dfdu can't read back is breaking.
- **Scope**: the crate without its `dfdu-` prefix (`tree`, `scan`, `engine`, `clean`, `rules`, `dupes`, `store`, `session`, `sys`, `update`, `tui`, `gui`, `sim`), or `cli` for `crates/dfdu`, `xtask`, `website`, `release`, `deps`. Leave it out when a change spans several.
- **Summary**: imperative, lower case after the colon, no trailing period, at most about 72 characters. Say what changes for the user: `fix(clean): keep a staged item when its parent is renamed`, not `fix: bug in staging`.
- One logical change per commit. Don't mix a `feat` with an unrelated `refactor`.

## Working in the repository

- Stage and commit explicit paths. Never `git commit -a` or `git add -A`: the maintainer edits files in the working tree alongside you.
- Plans, design notes and scratch files go in `scratchpad/` (ignored), not in `docs/` or the repository root.
- Before committing, run `cargo xtask lint` and the tests for what you touched (`cargo xtask test`, or `cargo nextest run -p <crate>`). `cargo xtask ci --quick` is the local part of CI.
- Platform code: `cargo xtask windows` type-checks for Windows, `cargo xtask windows --test` runs the tests for Windows under Wine (`--host user@box` on a Linux machine), and `cargo xtask linux` runs the tests in a Linux container. Run the Windows tests before pushing a change to paths or to tests that build them: a tree joins names with `\` on Windows, so a path written as `"/r/a"` in a test misses the tree's `/r\a` there. Build such paths with `HostPath::join` or take them from the tree.
- The FSEvents daemon tests are `#[ignore]`d because they flake under load; `cargo xtask test --daemon` runs them alone.
- Tests: unit tests and targeted integration tests. Don't add slow end-to-end suites driven by golden files or recordings. Pipeline changes (scan, engine, applier) get simulation coverage in `crates/dfdu-sim`; run more seeds with `cargo xtask sim --seeds 20000`.
- Generated files: the man pages and completions in `assets/` are checked byte for byte by `crates/dfdu/tests/assets.rs` (`cargo xtask release bump` regenerates them), and `.github/workflows/release.yml` comes from `dist generate` (configured in `dist-workspace.toml`). Don't edit either by hand.
- Workflows: pin actions to a full commit SHA with the version in a comment; `zizmor` runs in `cargo xtask lint`.
- Deleting is the dangerous part of this tool. Changes to `dfdu-clean`, protected paths in `dfdu-rules`, or the privileged helper in `dfdu-sys` need tests for the refusal cases too, not only the happy path.

## Writing

Docs, comments and user-facing text are plain and direct: short sentences, no marketing words, no em dashes. Doc comments say what something is or guarantees, not how it was arrived at. The website's docs are in `website/src/content/docs/docs/`; update them when behaviour, keys, options or rules change.
