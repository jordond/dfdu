//! Running commands: cargo through the rustup proxy, docker with the repository mounted, and
//! a list of steps that all run and are summed up at the end.

use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Instant;

use anyhow::{Context, bail};

pub(crate) type Step<'a> = (&'a str, Box<dyn FnOnce() -> anyhow::Result<()> + 'a>);

/// Runs every step even after one fails, then prints a summary and fails if any step did.
pub(crate) fn steps(list: Vec<Step>) -> anyhow::Result<()> {
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

pub(crate) fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

pub(crate) fn cargo<I: IntoIterator<Item = S>, S: AsRef<OsStr>>(args: I) -> Command {
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
pub(crate) fn docker_run(image: &str, target_volume: &str, privileged: bool) -> Command {
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

pub(crate) fn run(c: &mut Command) -> anyhow::Result<()> {
    eprintln!("$ {}", show(c));
    let status = c
        .status()
        .with_context(|| format!("starting {}", c.get_program().display()))?;
    if !status.success() {
        bail!("{} exited with {status}", show(c));
    }
    Ok(())
}

pub(crate) fn output(c: &mut Command) -> anyhow::Result<String> {
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
pub(crate) fn show(c: &Command) -> String {
    let env = c
        .get_envs()
        .filter_map(|(k, v)| Some(format!("{}={}", k.display(), quote(&v?.to_string_lossy()))));
    let prog = std::iter::once(c.get_program().to_string_lossy().into_owned());
    let args = c.get_args().map(|a| quote(&a.to_string_lossy()));
    env.chain(prog).chain(args).collect::<Vec<_>>().join(" ")
}

pub(crate) fn quote(s: &str) -> String {
    let plain = !s.is_empty()
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"-_./=:+,@%".contains(&b));
    if plain {
        s.to_owned()
    } else {
        format!("'{}'", s.replace('\'', r"'\''"))
    }
}

pub(crate) fn has(program: &str) -> bool {
    Command::new(program)
        .arg("--version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

pub(crate) fn has_cargo(sub: &str) -> bool {
    cargo([sub, "--version"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

pub(crate) fn need(program: &str) -> anyhow::Result<()> {
    if !has(program) {
        bail!("{program} is not installed");
    }
    Ok(())
}

pub(crate) fn need_cargo(sub: &str) -> anyhow::Result<()> {
    if !has_cargo(sub) {
        bail!("cargo-{sub} is not installed (cargo install --locked cargo-{sub})");
    }
    Ok(())
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
}
