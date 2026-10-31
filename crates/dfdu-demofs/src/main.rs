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

fn main() -> anyhow::Result<()> {
    match Cli::parse().cmd {
        Cmd::Mount {
            fixture,
            dir,
            ready,
            root,
            at,
        } => {
            let fixture = Fixture::load(&fixture)?;
            let tree = match (root, at) {
                (Some(root), Some(at)) => {
                    let mut tree = Fixture::load(&root)?.build()?;
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
            println!("{}", faketime(&Fixture::load(&fixture)?, minus.as_deref())?);
            Ok(())
        }
        Cmd::Check { fixture } => {
            println!("{}", summary(&Fixture::load(&fixture)?.build()?));
            Ok(())
        }
    }
}

/// The fixture's `now`, `minus` before it if given, as `FAKETIME` takes it.
fn faketime(fixture: &Fixture, minus: Option<&str>) -> anyhow::Result<String> {
    let back = minus.map_or(Ok(0), parse_age)?;
    Ok(format!("@{}", format_time(fixture.now()? - back)))
}

/// What `check` prints: the entries below the root, their apparent size and allocated bytes.
fn summary(tree: &Tree) -> String {
    format!(
        "entries {}\napparent {}\nalloc {}",
        tree.live() - 1,
        tree.apparent(),
        tree.used()
    )
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

#[cfg(test)]
mod tests {
    use clap::CommandFactory as _;

    use super::*;

    const FIXTURE: &str = "now = \"2026-10-01T09:00:00Z\"\n[[entry]]\npath = \"a/b\"\nsize = 10\n";

    #[test]
    fn the_command_line_is_well_formed() {
        Cli::command().debug_assert();
    }

    #[test]
    fn root_and_at_come_together() {
        let parse = |args: &[&str]| {
            Cli::try_parse_from(["dfdu-demofs", "mount", "f", "d"].iter().chain(args))
        };
        assert!(parse(&[]).is_ok());
        assert!(parse(&["--root", "r.toml", "--at", "home/demo"]).is_ok());
        assert!(parse(&["--root", "r.toml"]).is_err());
        assert!(parse(&["--at", "home/demo"]).is_err());
    }

    #[test]
    fn faketime_counts_back_from_now() {
        let f = Fixture::parse(FIXTURE).unwrap();
        assert_eq!(faketime(&f, None).unwrap(), "@2026-10-01 09:00:00");
        assert_eq!(faketime(&f, Some("7d")).unwrap(), "@2026-09-24 09:00:00");
        assert!(faketime(&f, Some("a week")).is_err());
    }

    #[test]
    fn summary_counts_entries_below_the_root() {
        let tree = Fixture::parse(FIXTURE).unwrap().build().unwrap();
        assert_eq!(summary(&tree), "entries 2\napparent 4106\nalloc 12288");
    }
}
