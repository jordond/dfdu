# Contributing

Common tasks go through `cargo xtask` (`crates/xtask`), which runs the same commands as the CI jobs of the same names; `cargo xtask --help` lists them all.

```sh
cargo xtask lint                                  # fmt, clippy, docs, unused dependencies, cargo-deny, zizmor
cargo xtask test                                  # every test (nextest when installed), then doc tests
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

Fuzzing needs nightly and cargo-fuzz, or Docker with the `dfdu-fuzz` image: `cargo xtask fuzz snapshot --seconds 600` runs one target from its corpus, seeds (`fuzz/seeds`) and regressions; without a target it runs each in turn, and `--list` names them. Releases: `cargo xtask release check` (assets, advisories, `dist plan`), `cargo xtask release bump 0.2.0 --commit` (version, lockfile, man pages and completions, commit and tag; pushing the tag publishes), and `cargo xtask release manifests 0.2.0` for the Scoop, winget and AUR files.

The site at [dfdu.app](https://dfdu.app) is `website/`: an Astro landing page and the Starlight docs, whose pages (the rules, themes and benchmarks guides among them) are in `website/src/content/docs/docs/`. It serves the GIFs and icons from `assets/`, copied at build time. `.github/workflows/website.yml` deploys `main` to Cloudflare and comments a preview URL on pull requests; `cargo xtask website deploy` does the same by hand (`--preview pr-12` for a preview).

The simulation (`crates/dfdu-sim`) is the main test of the data pipeline: it runs the real worker, watchdog and applier code against an in-memory filesystem with injected faults, random rescans, deletes and cancels, in a seeded schedule, and compares the result with the filesystem. CI runs 5,000 seeds per suite on every push.
