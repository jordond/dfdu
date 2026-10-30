//! `cargo xtask website`: the same commands as `.github/workflows/website.yml`.

use std::process::Command;

use anyhow::bail;
use clap::Subcommand;

use crate::cmd::{need, run, steps};

const DIR: &str = "website";

#[derive(Subcommand)]
pub(crate) enum Website {
    /// Install the dependencies from the lockfile.
    Install,
    /// Serve the site with live reload on http://localhost:4321.
    Dev,
    /// Type-check the site.
    Check,
    /// Build the site into website/dist.
    Build {
        /// Build a pull request preview: noindex everywhere, robots.txt disallows all.
        #[arg(long)]
        preview: bool,
    },
    /// Build, then serve website/dist as Cloudflare would (headers, 404 page) with wrangler dev.
    Serve,
    /// Check and build, as CI does.
    Ci,
    /// Render public/og.png, the social card, from the logo.
    Og,
    /// Build and deploy to dfdu.app (needs `wrangler login` or `CLOUDFLARE_API_TOKEN`).
    Deploy {
        /// Upload a preview version under this alias instead (pr-12 serves at pr-12-dfdu-web.*.workers.dev).
        #[arg(long, value_name = "ALIAS")]
        preview: Option<String>,
    },
}

pub(crate) fn website(w: Website) -> anyhow::Result<()> {
    need("bun")?;
    need("node")?;
    match w {
        Website::Install => install(),
        Website::Dev => bun(&["run", "dev"]),
        Website::Check => check(),
        Website::Build { preview } => build(preview),
        Website::Serve => {
            build(false)?;
            bun(&["x", "wrangler", "dev"])
        }
        Website::Ci => steps(vec![
            ("install", Box::new(install)),
            ("check", Box::new(check)),
            ("build", Box::new(|| build(false))),
        ]),
        Website::Og => bun(&["run", "og"]),
        Website::Deploy { preview: None } => {
            build(false)?;
            bun(&["x", "wrangler", "deploy"])
        }
        Website::Deploy {
            preview: Some(alias),
        } => {
            if !preview_alias_ok(&alias) {
                bail!(
                    "a preview alias is lowercase letters, digits and dashes, starting with a letter: {alias:?}"
                );
            }
            build(true)?;
            bun(&[
                "x",
                "wrangler",
                "versions",
                "upload",
                "--preview-alias",
                &alias,
            ])
        }
    }
}

/// Cloudflare's rule: lowercase letters, digits and dashes, starting with a letter, and a DNS
/// label with the Worker's name.
fn preview_alias_ok(alias: &str) -> bool {
    alias.starts_with(|c: char| c.is_ascii_lowercase())
        && alias.len() + "-dfdu-web".len() <= 63
        && alias
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

fn install() -> anyhow::Result<()> {
    bun(&["install", "--frozen-lockfile"])
}

fn check() -> anyhow::Result<()> {
    bun(&["run", "check"])
}

fn build(preview: bool) -> anyhow::Result<()> {
    let mut c = Command::new("bun");
    c.current_dir(DIR).args(["run", "build"]);
    if preview {
        c.env("DFDU_SITE_ENV", "preview");
    } else {
        c.env_remove("DFDU_SITE_ENV");
    }
    run(&mut c)
}

fn bun(args: &[&str]) -> anyhow::Result<()> {
    run(Command::new("bun").current_dir(DIR).args(args))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preview_aliases_follow_cloudflares_rules() {
        assert!(preview_alias_ok("pr-12"));
        assert!(!preview_alias_ok("12"));
        assert!(!preview_alias_ok("PR-12"));
        assert!(!preview_alias_ok("pr_12"));
        assert!(!preview_alias_ok(&"a".repeat(60)));
    }
}
