//! `cargo xtask release`: the dist plan, the release checks, the version bump and the package
//! manager manifests.

use std::fs;
use std::path::PathBuf;
use std::process::Command;

use anyhow::{Context, bail};
use clap::Subcommand;

use crate::cmd::{cargo, need, output, run, steps};
use crate::{assets, deny, toml_value};

#[derive(Subcommand)]
pub(crate) enum Release {
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

pub(crate) fn release(r: Release) -> anyhow::Result<()> {
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

/// Whether `version` reads as a version like 1.2.3: three or more parts, a numeric major.
fn is_version(version: &str) -> bool {
    version.split('.').count() >= 3
        && version
            .split(['.', '-', '+'])
            .next()
            .is_some_and(|m| m.parse::<u64>().is_ok())
}

/// `manifest` with its first `version = "old"` set to `new`: [workspace.package]'s, since
/// dependencies use inline tables.
fn set_version(manifest: &str, old: &str, new: &str) -> String {
    let line = format!("version = \"{old}\"");
    manifest.replacen(&line, &format!("version = \"{new}\""), 1)
}

fn bump(version: &str, commit: bool) -> anyhow::Result<()> {
    if !is_version(version) {
        bail!("{version:?} is not a version like 1.2.3");
    }
    if commit && !output(Command::new("git").args(["status", "--porcelain"]))?.is_empty() {
        bail!("the working tree has changes; commit or stash them first");
    }
    let manifest = fs::read_to_string("Cargo.toml")?;
    let old = toml_value(&manifest, "version").context("no version in Cargo.toml")?;
    fs::write("Cargo.toml", set_version(&manifest, &old, version))?;
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions_need_three_parts_and_a_numeric_major() {
        for v in ["0.2.0", "1.2.3-rc.1", "10.0.0+build"] {
            assert!(is_version(v), "{v}");
        }
        for v in ["1.2", "v1.2.3", "", "x.2.3"] {
            assert!(!is_version(v), "{v}");
        }
    }

    #[test]
    fn only_the_workspace_version_is_set() {
        let manifest = "[workspace.package]\nversion = \"0.1.0\"\n\n[workspace.dependencies]\n\
                        dfdu-tree = { path = \"crates/dfdu-tree\", version = \"0.1.0\" }\n";
        let got = set_version(manifest, "0.1.0", "0.2.0");
        assert_eq!(toml_value(&got, "version").as_deref(), Some("0.2.0"));
        assert!(got.contains("version = \"0.1.0\" }"));
    }
}
