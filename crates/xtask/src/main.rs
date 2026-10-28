//! `cargo xtask`: the repository's common tasks in one place. Each task runs the same commands
//! as the CI job of the same name (`.github/workflows/ci.yml`), so a local run reproduces CI.
//! Pinned values (the nightly for fuzzing, the coverage floor, the MSRV) are read from ci.yml
//! and Cargo.toml rather than repeated here.

use std::ffi::OsStr;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Instant;

use anyhow::{Context, bail};
use clap::{Args, Parser, Subcommand};

#[derive(Parser)]
#[command(about, bin_name = "cargo xtask")]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Format the workspace (`--check` only reports).
    Fmt {
        #[arg(long)]
        check: bool,
    },
    /// Clippy on every target and feature, warnings as errors.
    Clippy {
        /// Apply clippy's suggestions.
        #[arg(long)]
        fix: bool,
    },
    /// Build the docs, warnings as errors.
    Doc {
        #[arg(long)]
        open: bool,
    },
    /// The fast checks: fmt, clippy, docs, unused dependencies and cargo-deny.
    Lint,
    /// Find unused dependencies.
    Machete,
    /// cargo-deny: bans, licenses and sources (`--advisories` for RustSec advisories).
    Deny {
        #[arg(long)]
        advisories: bool,
    },
    /// Tests with nextest (cargo test when it is not installed), then doc tests.
    Test(TestArgs),
    /// The deterministic simulation (crates/dfdu-sim).
    Sim(SimArgs),
    /// The loom model tests, in their own target directory so the main build is kept.
    Loom,
    /// Kill dfdu at random points while it writes and deletes, and check recovery.
    Crash {
        #[arg(long, default_value_t = 40)]
        rounds: u32,
    },
    /// Tests with line coverage, failing below ci.yml's floor.
    Coverage {
        /// Also write an HTML report to target/llvm-cov/html.
        #[arg(long)]
        html: bool,
        /// Open the HTML report.
        #[arg(long)]
        open: bool,
        /// Do not fail below the floor (CI reports first, then checks).
        #[arg(long)]
        no_fail: bool,
    },
    /// Check the workspace builds on the MSRV from Cargo.toml.
    Msrv,
    /// Fuzz one target, or every target in turn. Uses Docker when cargo-fuzz is not installed.
    Fuzz(FuzzArgs),
    /// Run a command in a Linux container (default: the workspace tests).
    Linux(LinuxArgs),
    /// Type-check dfdu for Windows (x86_64-pc-windows-msvc) without the bundled SQLite build.
    Windows,
    /// Run the local part of CI and print a summary.
    Ci(CiArgs),
    /// Time dfdu against other disk usage tools (scripts/bench.sh; arguments are passed on).
    Bench {
        #[arg(trailing_var_arg = true, allow_hyphen_values = true, required = true)]
        args: Vec<String>,
    },
    /// Rewrite the man pages and completions in assets/.
    Assets,
    /// Record the README demo (assets/screenshots/browse.gif) with vhs.
    Demo,
    /// Release tasks.
    #[command(subcommand)]
    Release(Release),
}

#[derive(Args)]
struct TestArgs {
    /// Only these packages.
    #[arg(short, long = "package")]
    packages: Vec<String>,
    /// CI's profiles: optimised with debug assertions, retries for flaky tests.
    #[arg(long)]
    ci: bool,
    /// Skip the doc tests.
    #[arg(long)]
    no_doc: bool,
    /// Passed to the test runner.
    #[arg(last = true)]
    args: Vec<String>,
}

#[derive(Args)]
struct SimArgs {
    /// Seeds per suite (CI runs 5000).
    #[arg(long)]
    seeds: Option<u64>,
    /// Rerun one seed.
    #[arg(long)]
    seed: Option<u64>,
    /// Trace one path through the run (with --seed); shows the output.
    #[arg(long)]
    trace: Option<String>,
    /// Passed to the test binary.
    #[arg(last = true)]
    args: Vec<String>,
}

#[derive(Args)]
struct FuzzArgs {
    /// The target (`--list` for the names); every target when left out.
    target: Option<String>,
    /// Seconds per target.
    #[arg(long, default_value_t = 60)]
    seconds: u64,
    /// List the targets.
    #[arg(long)]
    list: bool,
    /// Run in the dfdu-fuzz Docker image even when cargo-fuzz is installed.
    #[arg(long)]
    docker: bool,
}

#[derive(Args)]
struct LinuxArgs {
    /// Needed by tests that mount (tmpfs, FUSE, btrfs).
    #[arg(long)]
    privileged: bool,
    #[arg(long, default_value = "rust:1.99")]
    image: String,
    /// The command (default: cargo test --workspace --no-fail-fast).
    #[arg(last = true)]
    cmd: Vec<String>,
}

#[derive(Args)]
struct CiArgs {
    /// Only fmt, clippy and the tests.
    #[arg(long, conflicts_with = "full")]
    quick: bool,
    /// Also the MSRV, coverage, CI's 5000 simulation seeds and a minute of fuzzing per target.
    #[arg(long)]
    full: bool,
}

#[derive(Subcommand)]
enum Release {
    /// What dist would build and publish for a tag.
    Plan,
    /// The checks a release needs on top of CI: assets current, advisories, dist plan.
    Check,
    /// Set the workspace version, update Cargo.lock and the assets; with --commit, commit and tag.
    Bump {
        /// The new version, e.g. 0.2.0.
        version: String,
        #[arg(long)]
        commit: bool,
    },
    /// Fill the Scoop, winget and AUR manifests for a published release (scripts/package-manifests.sh).
    Manifests {
        version: String,
        /// Output directory (default target/pkg/VERSION).
        out: Option<PathBuf>,
    },
}

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    std::env::set_current_dir(root())?;
    match cli.cmd {
        Cmd::Fmt { check } => fmt(check),
        Cmd::Clippy { fix } => clippy(fix),
        Cmd::Doc { open } => doc(open),
        Cmd::Lint => lint(),
        Cmd::Deny { advisories } => deny(advisories),
        Cmd::Machete => machete(),
        Cmd::Test(a) => test(&a),
        Cmd::Sim(a) => sim(&a),
        Cmd::Loom => loom(),
        Cmd::Crash { rounds } => run(cargo(["test", "--locked", "-p", "dfdu", "--test", "crash"])
            .env("DFDU_CRASH_ROUNDS", rounds.to_string())),
        Cmd::Coverage {
            html,
            open,
            no_fail,
        } => coverage(html, open, no_fail),
        Cmd::Msrv => msrv(),
        Cmd::Fuzz(a) => fuzz(&a),
        Cmd::Linux(a) => linux(&a),
        Cmd::Windows => windows(),
        Cmd::Ci(a) => ci(&a),
        Cmd::Bench { args } => run(Command::new("scripts/bench.sh").args(args)),
        Cmd::Assets => assets(),
        Cmd::Demo => demo(),
        Cmd::Release(r) => release(r),
    }
}

fn fmt(check: bool) -> anyhow::Result<()> {
    let mut c = cargo(["fmt", "--all"]);
    if check {
        c.arg("--check");
    }
    run(&mut c)
}

fn clippy(fix: bool) -> anyhow::Result<()> {
    let mut c = cargo(["clippy", "--workspace", "--all-targets", "--all-features"]);
    if fix {
        c.args(["--fix", "--allow-dirty", "--allow-staged"]);
    } else {
        c.arg("--locked");
    }
    run(c.args(["--", "-D", "warnings"]))
}

fn doc(open: bool) -> anyhow::Result<()> {
    let mut c = cargo([
        "doc",
        "--workspace",
        "--all-features",
        "--no-deps",
        "--locked",
    ]);
    if open {
        c.arg("--open");
    }
    run(c.env("RUSTDOCFLAGS", "-D warnings"))
}

fn lint() -> anyhow::Result<()> {
    steps(vec![
        ("fmt", Box::new(|| fmt(true))),
        ("clippy", Box::new(|| clippy(false))),
        ("doc", Box::new(|| doc(false))),
        ("machete", Box::new(machete)),
        ("deny", Box::new(|| deny(false))),
        ("zizmor", Box::new(zizmor)),
    ])
}

fn deny(advisories: bool) -> anyhow::Result<()> {
    need_cargo("deny")?;
    let checks: &[&str] = if advisories {
        &["advisories"]
    } else {
        &["bans", "licenses", "sources"]
    };
    run(cargo(["deny", "check"]).args(checks))
}

fn machete() -> anyhow::Result<()> {
    need_cargo("machete")?;
    run(&mut cargo(["machete"]))
}

fn zizmor() -> anyhow::Result<()> {
    need("zizmor")?;
    run(Command::new("zizmor").args([".github/workflows"]))
}

fn test(a: &TestArgs) -> anyhow::Result<()> {
    let packages = a.packages.iter().flat_map(|p| ["-p", p]);
    let scope: Vec<&str> = if a.packages.is_empty() {
        vec!["--workspace"]
    } else {
        packages.collect()
    };
    let nextest = has_cargo("nextest");
    let mut c = if nextest {
        let mut c = cargo(["nextest", "run"]);
        c.args(&scope);
        if a.ci {
            c.args(["--cargo-profile", "ci", "--profile", "ci", "--locked"]);
        }
        c
    } else {
        eprintln!("cargo-nextest not installed; using cargo test");
        let mut c = cargo(["test"]);
        c.args(&scope);
        if a.ci {
            c.args(["--profile", "ci", "--locked"]);
        }
        c
    };
    if !a.args.is_empty() {
        c.arg("--").args(&a.args);
    }
    run(&mut c)?;
    // nextest does not run doc tests; cargo test already did.
    if nextest && !a.no_doc {
        let mut c = cargo(["test", "--doc"]);
        c.args(&scope);
        if a.ci {
            c.args(["--profile", "ci", "--locked"]);
        }
        run(&mut c)?;
    }
    Ok(())
}

fn sim(a: &SimArgs) -> anyhow::Result<()> {
    let mut c = cargo(["test", "--profile", "ci", "--locked", "-p", "dfdu-sim"]);
    if let Some(n) = a.seeds {
        c.env("DFDU_SIM_SEEDS", n.to_string());
    }
    if let Some(s) = a.seed {
        c.env("DFDU_SIM_SEED", s.to_string());
    }
    let mut args = a.args.clone();
    if let Some(t) = &a.trace {
        if a.seed.is_none() {
            bail!("--trace needs --seed");
        }
        c.env("DFDU_SIM_TRACE", t);
        args.push("--nocapture".into());
    }
    if !args.is_empty() {
        c.arg("--").args(args);
    }
    run(&mut c)
}

fn loom() -> anyhow::Result<()> {
    run(cargo([
        "test",
        "--profile",
        "ci",
        "--locked",
        "-p",
        "dfdu-scan",
        "-p",
        "dfdu-engine",
        "--test",
        "loom",
    ])
    .env("RUSTFLAGS", "--cfg loom")
    .env("CARGO_TARGET_DIR", "target/loom"))
}

fn coverage(html: bool, open: bool, no_fail: bool) -> anyhow::Result<()> {
    need_cargo("llvm-cov")?;
    need_cargo("nextest")?;
    let min = ci_value("MIN_LINES")?;
    run(&mut cargo([
        "llvm-cov",
        "nextest",
        "--workspace",
        "--exclude",
        "xtask",
        "--all-features",
        "--locked",
        "--no-report",
        "--profile",
        "ci",
    ]))?;
    if html || open {
        let mut c = cargo(["llvm-cov", "report", "--html"]);
        if open {
            c.arg("--open");
        }
        run(&mut c)?;
    }
    if no_fail {
        return Ok(());
    }
    run(&mut cargo([
        "llvm-cov",
        "report",
        "--summary-only",
        "--fail-under-lines",
        &min,
    ]))
}

fn msrv() -> anyhow::Result<()> {
    let manifest = fs::read_to_string("Cargo.toml")?;
    let msrv = toml_value(&manifest, "rust-version").context("no rust-version in Cargo.toml")?;
    run(&mut cargo([
        &format!("+{msrv}"),
        "check",
        "--workspace",
        "--all-targets",
        "--all-features",
        "--locked",
    ]))
}

fn fuzz(a: &FuzzArgs) -> anyhow::Result<()> {
    let docker = a.docker || !has_cargo("fuzz");
    // The dfdu-fuzz image has one nightly; on the host, CI's pinned one.
    let nightly = if docker {
        "nightly".to_owned()
    } else {
        ci_value("NIGHTLY")?
    };
    let script = match (&a.target, a.list) {
        (_, true) => format!("cd fuzz && cargo +{nightly} fuzz list"),
        (None, false) => format!(".github/scripts/fuzz.sh {nightly} {}", a.seconds),
        (Some(t), false) => {
            let dirs: Vec<String> = ["corpus", "seeds", "regressions"]
                .iter()
                .map(|d| format!("{d}/{t}"))
                .filter(|d| d.starts_with("corpus") || Path::new("fuzz").join(d).is_dir())
                .collect();
            format!(
                "cd fuzz && mkdir -p corpus/{t} && cargo +{nightly} fuzz run {} {} -- \
                 -max_total_time={} -timeout=10 -rss_limit_mb=2048",
                quote(t),
                dirs.iter().map(|d| quote(d)).collect::<Vec<_>>().join(" "),
                a.seconds,
            )
        }
    };
    if docker {
        eprintln!("running in the dfdu-fuzz Docker image");
        need("docker")?;
        run(docker_run("dfdu-fuzz", "dfdu-fuzz-target", false).args(["bash", "-c", &script]))
    } else {
        run(Command::new("bash").args(["-c", &script]))
    }
}

fn linux(a: &LinuxArgs) -> anyhow::Result<()> {
    need("docker")?;
    let cmd = if a.cmd.is_empty() {
        "cargo test --workspace --no-fail-fast".to_owned()
    } else {
        a.cmd.iter().map(|s| quote(s)).collect::<Vec<_>>().join(" ")
    };
    // The image has no git; the remote classification test runs `git init`.
    let script = format!("apt-get -qq update && apt-get -qq install -y git >/dev/null && {cmd}");
    run(docker_run(&a.image, "dfdu-target", a.privileged).args(["bash", "-c", &script]))
}

fn windows() -> anyhow::Result<()> {
    const TARGET: &str = "x86_64-pc-windows-msvc";
    let installed = output(Command::new("rustup").args(["target", "list", "--installed"]))?;
    if !installed.lines().any(|l| l == TARGET) {
        bail!("the {TARGET} target is not installed: rustup target add {TARGET}");
    }
    // `check` never links, so pointing libsqlite3-sys at an empty directory skips its C build.
    run(cargo(["check", "--target", TARGET, "-p", "dfdu"])
        .env("LIBSQLITE3_SYS_USE_PKG_CONFIG", "1")
        .env("SQLITE3_LIB_DIR", std::env::temp_dir()))
}

fn ci(a: &CiArgs) -> anyhow::Result<()> {
    let test_args = TestArgs {
        packages: vec![],
        ci: true,
        no_doc: false,
        args: vec![],
    };
    let mut list: Vec<Step> = vec![
        ("fmt", Box::new(|| fmt(true))),
        ("clippy", Box::new(|| clippy(false))),
        ("test", Box::new(move || test(&test_args))),
    ];
    if !a.quick {
        let seeds = a.full.then_some(5000);
        list.extend::<Vec<Step>>(vec![
            ("doc", Box::new(|| doc(false))),
            (
                "simulation",
                Box::new(move || {
                    sim(&SimArgs {
                        seeds,
                        seed: None,
                        trace: None,
                        args: vec![],
                    })
                }),
            ),
            ("loom", Box::new(loom)),
            ("deny", Box::new(|| deny(false))),
            ("machete", Box::new(machete)),
            ("zizmor", Box::new(zizmor)),
        ]);
    }
    if a.full {
        list.extend::<Vec<Step>>(vec![
            ("msrv", Box::new(msrv)),
            ("coverage", Box::new(|| coverage(false, false, false))),
            (
                "fuzz",
                Box::new(|| {
                    fuzz(&FuzzArgs {
                        target: None,
                        seconds: 60,
                        list: false,
                        docker: false,
                    })
                }),
            ),
        ]);
    }
    steps(list)
}

fn assets() -> anyhow::Result<()> {
    run(cargo(["test", "-p", "dfdu", "--test", "assets"]).env("DFDU_UPDATE_ASSETS", "1"))
}

fn demo() -> anyhow::Result<()> {
    need("vhs")?;
    run(&mut cargo(["build", "--release", "-p", "dfdu"]))?;
    // browse.tape reads the tree from this path.
    run(Command::new("scripts/vhs/demo-tree.sh").arg("/tmp/dfdu-demo"))?;
    run(Command::new("vhs").arg("scripts/vhs/browse.tape"))
}

fn release(r: Release) -> anyhow::Result<()> {
    match r {
        Release::Plan => {
            need("dist")?;
            run(Command::new("dist").arg("plan"))
        }
        Release::Check => steps(vec![
            (
                "assets",
                Box::new(|| run(&mut cargo(["test", "-p", "dfdu", "--test", "assets"]))),
            ),
            ("advisories", Box::new(|| deny(true))),
            ("dist plan", Box::new(|| release(Release::Plan))),
        ]),
        Release::Bump { version, commit } => bump(&version, commit),
        Release::Manifests { version, out } => {
            need("gh")?;
            let mut c = Command::new("scripts/package-manifests.sh");
            c.arg(&version);
            if let Some(o) = out {
                c.arg(o);
            }
            run(&mut c)
        }
    }
}

fn bump(version: &str, commit: bool) -> anyhow::Result<()> {
    let ok = version.split('.').count() >= 3
        && version
            .split(['.', '-', '+'])
            .next()
            .is_some_and(|m| m.parse::<u64>().is_ok());
    if !ok {
        bail!("{version:?} is not a version like 1.2.3");
    }
    if commit && !output(Command::new("git").args(["status", "--porcelain"]))?.is_empty() {
        bail!("the working tree has changes; commit or stash them first");
    }
    let manifest = fs::read_to_string("Cargo.toml")?;
    let old = toml_value(&manifest, "version").context("no version in Cargo.toml")?;
    // The first `version = ` is [workspace.package]'s; dependencies use inline tables.
    let line = format!("version = \"{old}\"");
    fs::write(
        "Cargo.toml",
        manifest.replacen(&line, &format!("version = \"{version}\""), 1),
    )?;
    eprintln!("version {old} -> {version}");
    run(&mut cargo(["update", "--workspace"]))?;
    assets()?;
    if commit {
        let tag = format!("v{version}");
        run(Command::new("git").args(["commit", "-am", &format!("Release {tag}")]))?;
        run(Command::new("git").args(["tag", "-a", &tag, "-m", &tag]))?;
        eprintln!("\npush to publish: git push origin HEAD {tag}");
    }
    Ok(())
}

type Step<'a> = (&'a str, Box<dyn FnOnce() -> anyhow::Result<()> + 'a>);

/// Runs every step even after one fails, then prints a summary and fails if any step did.
fn steps(list: Vec<Step>) -> anyhow::Result<()> {
    let mut results = Vec::new();
    for (name, f) in list {
        eprintln!("\n==> {name}");
        let start = Instant::now();
        let r = f();
        if let Err(e) = &r {
            eprintln!("{name}: {e:#}");
        }
        results.push((name, r.is_ok(), start.elapsed()));
    }
    eprintln!("\n{:<12} {:<6} time", "step", "result");
    for (name, ok, t) in &results {
        eprintln!(
            "{name:<12} {:<6} {:.0?}",
            if *ok { "ok" } else { "FAILED" },
            t
        );
    }
    let failed: Vec<_> = results.iter().filter(|r| !r.1).map(|r| r.0).collect();
    if !failed.is_empty() {
        bail!("failed: {}", failed.join(", "));
    }
    Ok(())
}

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn cargo<I: IntoIterator<Item = S>, S: AsRef<OsStr>>(args: I) -> Command {
    // The rustup proxy, not $CARGO (one toolchain's binary), so `cargo +1.95` works.
    let mut c = Command::new("cargo");
    // What `cargo run` set for xtask; cargo-machete reads CARGO_PKG_NAME as "run by cargo".
    for (k, _) in std::env::vars_os() {
        if k.to_str()
            .is_some_and(|k| k.starts_with("CARGO_PKG_") || k.starts_with("CARGO_MANIFEST_"))
        {
            c.env_remove(k);
        }
    }
    c.args(args);
    c
}

/// A `docker run` of `image` with the repository at /src, the cargo registry and a target
/// directory in named volumes, so builds are kept between runs.
fn docker_run(image: &str, target_volume: &str, privileged: bool) -> Command {
    let mut c = Command::new("docker");
    c.args(["run", "--rm"]);
    if privileged {
        c.arg("--privileged");
    }
    let src = root().canonicalize().unwrap_or_else(|_| root());
    c.arg("-v").arg(format!("{}:/src", src.display()));
    c.args(["-v", "dfdu-cargo:/usr/local/cargo/registry"]);
    c.arg("-v").arg(format!("{target_volume}:/target"));
    c.args(["-e", "CARGO_TARGET_DIR=/target", "-w", "/src", image]);
    c
}

fn run(c: &mut Command) -> anyhow::Result<()> {
    eprintln!("$ {}", show(c));
    let status = c
        .status()
        .with_context(|| format!("starting {}", c.get_program().display()))?;
    if !status.success() {
        bail!("{} exited with {status}", show(c));
    }
    Ok(())
}

fn output(c: &mut Command) -> anyhow::Result<String> {
    let o = c
        .stderr(Stdio::inherit())
        .output()
        .with_context(|| format!("starting {}", show(c)))?;
    if !o.status.success() {
        bail!("{} exited with {}", show(c), o.status);
    }
    Ok(String::from_utf8_lossy(&o.stdout).trim().to_owned())
}

/// The command line, with the environment it sets, as it would be typed.
fn show(c: &Command) -> String {
    let env = c
        .get_envs()
        .filter_map(|(k, v)| Some(format!("{}={}", k.display(), quote(&v?.to_string_lossy()))));
    let prog = std::iter::once(c.get_program().to_string_lossy().into_owned());
    let args = c.get_args().map(|a| quote(&a.to_string_lossy()));
    env.chain(prog).chain(args).collect::<Vec<_>>().join(" ")
}

fn quote(s: &str) -> String {
    let plain = !s.is_empty()
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"-_./=:+,@%".contains(&b));
    if plain {
        s.to_owned()
    } else {
        format!("'{}'", s.replace('\'', r"'\''"))
    }
}

fn has(program: &str) -> bool {
    Command::new(program)
        .arg("--version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

fn has_cargo(sub: &str) -> bool {
    cargo([sub, "--version"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

fn need(program: &str) -> anyhow::Result<()> {
    if !has(program) {
        bail!("{program} is not installed");
    }
    Ok(())
}

fn need_cargo(sub: &str) -> anyhow::Result<()> {
    if !has_cargo(sub) {
        bail!("cargo-{sub} is not installed (cargo install --locked cargo-{sub})");
    }
    Ok(())
}

/// A `KEY: value` from ci.yml's `env:` blocks.
fn ci_value(key: &str) -> anyhow::Result<String> {
    let yml = fs::read_to_string(".github/workflows/ci.yml")?;
    yml.lines()
        .find_map(|l| l.trim().strip_prefix(key)?.strip_prefix(':'))
        .map(|v| v.trim().trim_matches('"').to_owned())
        .with_context(|| format!("no {key} in ci.yml"))
}

/// The first `key = "value"` in a TOML file.
fn toml_value(toml: &str, key: &str) -> Option<String> {
    toml.lines().find_map(|l| {
        let v = l.strip_prefix(key)?.trim_start().strip_prefix('=')?.trim();
        Some(v.strip_prefix('"')?.strip_suffix('"')?.to_owned())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quotes_only_what_the_shell_would_split() {
        assert_eq!(quote("--cfg"), "--cfg");
        assert_eq!(quote("--cfg loom"), "'--cfg loom'");
        assert_eq!(quote("it's"), r"'it'\''s'");
        assert_eq!(quote(""), "''");
    }

    #[test]
    fn reads_the_first_toml_value() {
        let toml = "[workspace.package]\nversion = \"0.1.0\"\nrust-version = \"1.95\"\n";
        assert_eq!(toml_value(toml, "version").as_deref(), Some("0.1.0"));
        assert_eq!(toml_value(toml, "rust-version").as_deref(), Some("1.95"));
        assert_eq!(toml_value(toml, "edition"), None);
    }

    #[test]
    fn ci_pins_are_found() {
        std::env::set_current_dir(root()).unwrap();
        assert!(ci_value("NIGHTLY").unwrap().starts_with("nightly-"));
        assert!(ci_value("MIN_LINES").unwrap().parse::<u32>().is_ok());
    }
}
