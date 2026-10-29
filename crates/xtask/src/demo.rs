//! `cargo xtask demo`: records the tapes in demo/tapes with VHS, each in its own container
//! from demo/Dockerfile, against a fixture mounted by dfdu-demofs (demo/README.md).
//!
//! The binaries are built for Linux in the `rust` image (or taken from `--bin-dir`) and copied
//! into each container with the tapes and fixtures; nothing is bind-mounted into a recording,
//! so its disk list shows no host directory, and Docker may be remote (`DOCKER_HOST`).

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, bail};
use clap::Args;

use crate::cmd::{docker_run, need, run};

const IMAGE: &str = "dfdu-demo";
/// Where the recordings are copied to.
const ASSETS: &str = "assets/demos";
/// The binaries a recording needs.
const BINS: [&str; 2] = ["dfdu", "dfdu-demofs"];
/// Where the Linux binaries are put when built here.
const BUILT: &str = "demo/out/bin";

#[derive(Args)]
pub(crate) struct DemoArgs {
    /// The tapes to record (names in demo/tapes); every tape when left out.
    tapes: Vec<String>,
    /// List the tapes.
    #[arg(long)]
    list: bool,
    /// Take dfdu and dfdu-demofs for Linux from this directory instead of building them.
    #[arg(long)]
    bin_dir: Option<PathBuf>,
    /// The image to build the binaries in.
    #[arg(long, default_value = "rust:1.99")]
    build_image: String,
}

/// Names of the tapes in demo/tapes, sorted; `_`-prefixed ones are included by the others.
fn tapes() -> anyhow::Result<Vec<String>> {
    let mut names: Vec<String> = fs::read_dir("demo/tapes")?
        .filter_map(|e| {
            let name = e.ok()?.file_name().into_string().ok()?;
            let stem = name.strip_suffix(".tape")?;
            (!stem.starts_with('_')).then(|| stem.to_owned())
        })
        .collect();
    names.sort();
    Ok(names)
}

pub(crate) fn demo(a: &DemoArgs) -> anyhow::Result<()> {
    let all = tapes()?;
    if a.list {
        for t in &all {
            println!("{t}");
        }
        return Ok(());
    }
    let chosen = if a.tapes.is_empty() {
        all.clone()
    } else {
        a.tapes.clone()
    };
    if let Some(t) = chosen.iter().find(|t| !all.contains(t)) {
        bail!("no tape {t:?} in demo/tapes (--list names them)");
    }
    need("docker")?;
    let bins = match &a.bin_dir {
        Some(d) => d.clone(),
        // The build bind-mounts this checkout, which a remote Docker does not have.
        None if std::env::var_os("DOCKER_HOST").is_some() => {
            bail!("with DOCKER_HOST set, build the Linux binaries elsewhere and pass --bin-dir")
        }
        None => build(&a.build_image)?,
    };
    for b in BINS {
        if !bins.join(b).is_file() {
            bail!("no {b} in {}", bins.display());
        }
    }
    run(Command::new("docker").args(["build", "-q", "-t", IMAGE, "demo"]))?;
    fs::create_dir_all(ASSETS)?;
    for t in &chosen {
        record(t, &bins).with_context(|| format!("recording {t}"))?;
    }
    eprintln!("recorded {} into {ASSETS}/", chosen.join(", "));
    Ok(())
}

/// Builds dfdu and dfdu-demofs for Linux (glibc, so libfaketime can set their clock) and
/// copies them to [`BUILT`].
fn build(image: &str) -> anyhow::Result<PathBuf> {
    let script = format!(
        "cargo build --locked --release -p dfdu -p dfdu-demofs \
         && mkdir -p {BUILT} && cp /target/release/dfdu /target/release/dfdu-demofs {BUILT}/"
    );
    run(docker_run(image, "dfdu-target", false).args(["bash", "-c", &script]))?;
    Ok(PathBuf::from(BUILT))
}

/// Records tape `name` in a new container and copies what it wrote to [`ASSETS`].
fn record(name: &str, bins: &Path) -> anyhow::Result<()> {
    let container = format!("dfdu-demo-{name}");
    // A container left by an interrupted run.
    let _ = Command::new("docker")
        .args(["rm", "-f", &container])
        .output();
    run(Command::new("docker").args([
        "create",
        "--privileged",
        "--hostname",
        "devbox",
        "--name",
        &container,
        IMAGE,
        name,
    ]))?;
    let result = (|| {
        let cp = |from: String, to: String| run(Command::new("docker").args(["cp", &from, &to]));
        for b in BINS {
            cp(
                bins.join(b).display().to_string(),
                format!("{container}:/opt/dfdu/bin/{b}"),
            )?;
        }
        for part in ["run.sh", "env.sh", "tapes", "fixtures"] {
            cp(format!("demo/{part}"), format!("{container}:/work/{part}"))?;
        }
        run(Command::new("docker").args(["start", "-a", &container]))?;
        cp(format!("{container}:/work/out/."), ASSETS.to_owned())
    })();
    let _ = Command::new("docker")
        .args(["rm", "-f", &container])
        .output();
    result
}
