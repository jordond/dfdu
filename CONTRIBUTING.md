# Contributing

Commits and pull request titles follow [Conventional Commits](https://www.conventionalcommits.org/) (`feat(tui): ...`, `fix(clean): ...`): the changelog and the version are generated from them. [AGENTS.md](AGENTS.md) lists the types and scopes.

Common tasks go through `cargo xtask` (`crates/xtask`), which runs the same commands as the CI jobs of the same names; `cargo xtask --help` lists them all.

```sh
cargo xtask lint                                  # fmt, clippy, docs, unused dependencies, cargo-deny, zizmor
cargo xtask test                                  # every test (nextest when installed), then doc tests
cargo xtask test --daemon                         # and, alone after them, the FSEvents tests (macOS; CI runs them)
cargo xtask ci                                    # the local part of CI with a summary (--quick, --full)
cargo xtask sim --seeds 20000                     # more simulation seeds
cargo xtask sim --seed 1234                       # rerun one seed
cargo xtask sim --seed 1234 --trace /sim/d0/f1    # trace one path through that seed
cargo xtask loom                                  # loom model tests
cargo xtask crash --rounds 40                     # kill dfdu while it writes and deletes, check recovery
cargo xtask coverage --html                       # line coverage, failing below CI's floor
cargo xtask linux --privileged                    # the workspace tests in a Linux container
cargo xtask windows                               # type-check for x86_64-pc-windows-msvc
cargo xtask demo                                  # record the demo GIFs (demo/README.md)
cargo xtask website dev                           # dfdu.app with live reload (website/, needs Bun and Node)
cargo xtask website serve                         # the built site as Cloudflare serves it
DFDU_CHECK=1 cargo run --release -- scan /usr     # check every invariant after every slice
```

Fuzzing needs nightly and cargo-fuzz, or Docker with the `dfdu-fuzz` image: `cargo xtask fuzz snapshot --seconds 600` runs one target from its corpus, seeds (`fuzz/seeds`) and regressions; without a target it runs each in turn, and `--list` names them. Releases: `cargo xtask release check` (assets, advisories, `dist plan`), `cargo xtask release bump --commit` (the version the conventional commits since the last tag call for, or the one given as in `bump 0.4.0`; lockfile, man pages and completions, the new `CHANGELOG.md` section from git-cliff, commit and tag; without `--commit` to edit the changelog first; pushing the tag publishes the GitHub release, and `dfdu` and the crates it needs to crates.io through `.github/workflows/publish-crates.yml` with Trusted Publishing), `cargo xtask release publish` (the same crates.io publish by hand with your token, for a new crate's first version, which trusted publishing cannot create; `--dry-run` packages and builds them without uploading), and `cargo xtask release manifests 0.2.0` for the Scoop, winget and AUR files.

The site at [dfdu.app](https://dfdu.app) is `website/`: an Astro landing page and the Starlight docs, whose pages (the rules, themes and benchmarks guides among them) are in `website/src/content/docs/docs/`. It serves the GIFs and icons from `assets/`, copied at build time. `.github/workflows/website.yml` deploys `main` to Cloudflare and comments a preview URL on pull requests; `cargo xtask website deploy` does the same by hand (`--preview pr-12` for a preview).
The simulation (`crates/dfdu-sim`) is the main test of the data pipeline: it runs the real worker, watchdog and applier code against an in-memory filesystem with injected faults, random rescans, deletes and cancels, in a seeded schedule, and compares the result with the filesystem. CI runs 5,000 seeds per suite on every push.

## The crates

How a scan flows through the crates, and how each part is tested, is described in [Architecture](https://dfdu.app/docs/architecture/).

| Crate | What it owns |
|---|---|
| `dfdu` | The binary: command line, subcommands, JSON output |
| `dfdu-tui` | The terminal browser |
| `dfdu-gui` | The desktop app |
| `dfdu-session` | What a front end shows besides drawing and input: views, marks, filters, the treemap, the cleanup view, actions, key effects |
| `dfdu-engine` | The applier (the tree's single writer), jobs, classifier and measurer pools, resume from the change journal, the remote agent and its protocol |
| `dfdu-scan` | The scanner: workers, backends, byte budget, watchdog, change journals (FSEvents, USN, btrfs) |
| `dfdu-tree` | The tree of scanned nodes: slots, generational references, size aggregation |
| `dfdu-rules` | Categories and cleanup rules: schema, loader, the built-in pack (`src/builtin.toml`), protected paths, matcher |
| `dfdu-clean` | Deleting: plans, verification against the scan, trash and permanent delete, journal |
| `dfdu-dupes` | The duplicate finder: size, then partial hash, then full hash |
| `dfdu-store` | Persistence: ncdu import and export, snapshots, the scan cache and history |
| `dfdu-sys` | Unsafe OS calls behind safe wrappers, the disk list, dfdu's directories, the macOS privileged helper |
| `dfdu-sim` | Seeded simulation of the scanner and applier with fault injection (tests only) |
| `dfdu-hangfs` | A FUSE filesystem that never answers, for watchdog tests (Linux, tests only) |
| `dfdu-demofs` | A FUSE filesystem with a made-up home directory, for the demo recordings (Linux) |
| `dfdu-bench` | Synthetic trees for the benchmarks |
| `xtask` | Repository tasks (`cargo xtask --help`) |
