//! `dfdu-demofs`: mounts a demo fixture, or tells what one holds.

use std::path::PathBuf;

use anyhow::Context as _;
use clap::{Parser, Subcommand};
use dfdu_demofs::{Fixture, Tree, format_time, parse_age};

#[derive(Parser)]
#[command(about = "Serve a made-up home directory for the demo recordings")]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Mount FIXTURE at DIR and serve it until unmounted (Linux; needs root or fusermount3).
    Mount {
        fixture: PathBuf,
        dir: PathBuf,
        /// Create this file once the mount answers.
        #[arg(long)]
        ready: Option<PathBuf>,
        /// Serve this fixture's tree with FIXTURE in it at `--at`; the disk and the time are
        /// still FIXTURE's.
        #[arg(long, requires = "at")]
        root: Option<PathBuf>,
        /// Where FIXTURE goes in the `--root` tree, e.g. `home/demo`.
        #[arg(long, requires = "root")]
        at: Option<String>,
    },
    /// Print the fixture's `now` as libfaketime's FAKETIME takes it (`@YYYY-MM-DD HH:MM:SS`).
    Now {
        fixture: PathBuf,
        /// That long before it (`7d`).
        #[arg(long)]
        minus: Option<String>,
    },
    /// Build the fixture and print its entries, apparent size and allocated bytes.
    Check { fixture: PathBuf },
}

fn read(path: &std::path::Path) -> anyhow::Result<Fixture> {
    Fixture::load(path)
}

fn main() -> anyhow::Result<()> {
    match Cli::parse().cmd {
        Cmd::Mount {
            fixture,
            dir,
            ready,
            root,
            at,
        } => {
            let fixture = read(&fixture)?;
            let tree = match (root, at) {
                (Some(root), Some(at)) => {
                    let mut tree = read(&root)?.build()?;
                    fixture
                        .build_at(&mut tree, &at)
                        .with_context(|| format!("at {at:?}"))?;
                    tree
                }
                _ => fixture.build()?,
            };
            mount(&fixture, tree, &dir, ready)
        }
        Cmd::Now { fixture, minus } => {
            let back = minus.as_deref().map_or(Ok(0), parse_age)?;
            println!("@{}", format_time(read(&fixture)?.now()? - back));
            Ok(())
        }
        Cmd::Check { fixture } => {
            let tree = read(&fixture)?.build()?;
            let apparent: u64 = (1..tree.nodes.len()).map(|n| tree.size(n)).sum();
            println!(
                "entries {}\napparent {apparent}\nalloc {}",
                tree.live() - 1,
                tree.used()
            );
            Ok(())
        }
    }
}

#[cfg(target_os = "linux")]
fn mount(
    fixture: &Fixture,
    tree: Tree,
    dir: &std::path::Path,
    ready: Option<PathBuf>,
) -> anyhow::Result<()> {
    let session = dfdu_demofs::mount_tree(fixture, tree, dir)?;
    if let Some(r) = ready {
        std::fs::write(&r, b"").with_context(|| format!("writing {}", r.display()))?;
    }
    // The session ends when the filesystem is unmounted.
    session.join()?;
    Ok(())
}

#[cfg(not(target_os = "linux"))]
fn mount(_: &Fixture, _: Tree, _: &std::path::Path, _: Option<PathBuf>) -> anyhow::Result<()> {
    anyhow::bail!("mounting needs Linux; record the demos with `cargo xtask demo`")
}
