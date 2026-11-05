//! `cargo xtask windows --test`: the workspace's tests built for Windows (x86_64-pc-windows-gnu)
//! and run under Wine in a Linux container, with Docker here or on another machine over ssh.
//! Windows' path rules, its standard library and the code behind `cfg(windows)` run as they do
//! on Windows, so a test that assumes `/` fails here as it does in CI. What Wine lacks is left
//! out by the `wine` nextest profile (`.config/nextest.toml`); real Windows (CI's
//! windows-latest) stays the reference.

use std::path::PathBuf;
use std::process::{Command, Stdio};

use anyhow::{Context, bail};
use clap::Args;

use crate::cmd::{need, quote, root, run};

/// The image's name, on whichever machine runs Docker.
const IMAGE: &str = "dfdu-wine";
/// Where the checkout is synced to on another machine, below its home directory.
const REMOTE_DIR: &str = "dfdu-xtask-wine";
/// What is synced: what git would commit, by the `.gitignore` files' rules, without `.git`.
/// The build output, corpora and node modules they leave out are hundreds of megabytes.
const SYNCED: [&str; 4] = ["--filter", ":- .gitignore", "--exclude", "/.git"];

#[derive(Args)]
pub struct WineArgs {
    /// Run on this machine over ssh (`user@host`), which needs Docker and rsync: faster than
    /// Docker emulating x86_64 on an ARM Mac. The checkout is synced to ~/dfdu-xtask-wine.
    #[arg(long, requires = "test")]
    host: Option<String>,
    /// The ssh key for `--host`, used alone (IdentitiesOnly).
    #[arg(long, requires = "host")]
    identity: Option<PathBuf>,
    /// Arguments for `cargo nextest run`, such as a test filter.
    #[arg(last = true)]
    args: Vec<String>,
}

/// Builds the image if it changed and runs the tests in it, here or on `--host`.
pub fn wine(a: &WineArgs) -> anyhow::Result<()> {
    let Some(host) = &a.host else {
        need("docker")?;
        let src = root().canonicalize().context("the checkout")?;
        return run(Command::new("sh")
            .arg("-c")
            .arg(script(&quote(&src.display().to_string()), &a.args)));
    };
    need("rsync")?;
    let ssh = ssh_command(a.identity.as_ref());
    let mut sync = Command::new("rsync");
    let shell: Vec<String> = ssh.iter().map(|s| quote(s)).collect();
    sync.args([
        "-az",
        "--delete",
        "--delete-excluded",
        "-e",
        &shell.join(" "),
    ]);
    sync.args(SYNCED);
    let mut from = root().canonicalize()?.into_os_string();
    from.push("/");
    sync.arg(from).arg(format!("{host}:{REMOTE_DIR}/"));
    run(&mut sync)?;
    // The script goes in on stdin: the remote login shell need not be one that reads it.
    let body = format!(
        "set -e\nexport DOCKER_CONFIG=\"$HOME/.cache/dfdu-xtask-docker\"\nmkdir -p \"$DOCKER_CONFIG\"\n{}",
        script(&format!("\"$HOME/{REMOTE_DIR}\""), &a.args)
    );
    let mut c = Command::new(&ssh[0]);
    c.args(&ssh[1..]).arg(host).args(["bash", "-s"]);
    eprintln!("$ {} {host} bash -s", shell.join(" "));
    let mut child = c.stdin(Stdio::piped()).spawn().context("starting ssh")?;
    std::io::Write::write_all(
        &mut child.stdin.take().context("ssh's stdin")?,
        body.as_bytes(),
    )?;
    let status = child.wait()?;
    if !status.success() {
        bail!("the tests on {host} exited with {status}");
    }
    Ok(())
}

/// ssh and its options, one argument each.
fn ssh_command(identity: Option<&PathBuf>) -> Vec<String> {
    let mut out = vec!["ssh".to_owned()];
    if let Some(key) = identity {
        out.extend(["-o", "IdentitiesOnly=yes", "-i"].map(String::from));
        out.push(key.display().to_string());
    }
    out
}

/// The shell script that builds the image and runs the tests with the checkout at `src` (a
/// shell word), passing `args` to nextest.
fn script(src: &str, args: &[String]) -> String {
    let args: Vec<String> = args.iter().map(|s| quote(s)).collect();
    format!(
        "docker build -q -t {IMAGE} - < {src}/crates/xtask/wine.Dockerfile >/dev/null\n\
         docker run --rm --platform linux/amd64 -v {src}:/src \
         -v dfdu-wine-cargo:/usr/local/cargo/registry -v dfdu-wine-target:/target \
         -e CARGO_TARGET_DIR=/target -e INSTA_UPDATE=no -e 'INSTA_WORKSPACE_ROOT=Z:\\src' \
         -w /src {IMAGE} \
         cargo nextest run --workspace --target x86_64-pc-windows-gnu --profile wine {}\n",
        args.join(" ")
    )
}

#[cfg(test)]
mod tests {
    use super::script;

    /// Insta cannot run `cargo metadata` from inside Wine, so it is told where the workspace
    /// is, as Wine names it; and the arguments reach nextest quoted.
    #[test]
    fn the_script_names_the_workspace_for_insta_and_passes_the_arguments() {
        let s = script("'/a b'", &["-E".into(), "test(a b)".into()]);
        assert!(s.contains(r"'INSTA_WORKSPACE_ROOT=Z:\src'"), "{s}");
        assert!(s.contains("--profile wine -E 'test(a b)'"), "{s}");
        assert!(s.contains("< '/a b'/crates/xtask/wine.Dockerfile"), "{s}");
        assert!(s.contains("-v '/a b':/src"), "{s}");
    }
}
