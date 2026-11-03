//! `dfdu update`: checks the latest release against this build and, unless `--check`, installs
//! it over this binary (checked against its published SHA-256), with the man pages and
//! completions install.sh put in place. A copy managed by Homebrew, Nix or the system's package
//! manager (Scoop and winget too) is left to it: the command to update it is printed instead.
//! Without feature `self-update` the command only says the build has no updates.

#[cfg(feature = "self-update")]
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use clap::Args;
#[cfg(feature = "self-update")]
use dfdu_update::cli::{self, ExtraDirs, Managed};
#[cfg(feature = "self-update")]
use dfdu_update::{Releases, Version};

#[derive(Args)]
pub struct UpdateArgs {
    /// Only check whether a newer release is out; change nothing.
    #[arg(long)]
    check: bool,
    /// Install the latest release even when this build is as new (to repair a copy).
    #[arg(long, conflicts_with = "check")]
    force: bool,
}

pub fn run(args: &UpdateArgs) -> ExitCode {
    #[cfg(not(feature = "self-update"))]
    let r: Result<(), String> = {
        let _ = (args.check, args.force);
        Err(
            "this dfdu was built without updates (feature `self-update`); update it the way \
             it was installed"
                .into(),
        )
    };
    #[cfg(feature = "self-update")]
    let r = update(args);
    match r {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("dfdu: {e}");
            ExitCode::from(1)
        }
    }
}

#[cfg(feature = "self-update")]
fn update(args: &UpdateArgs) -> Result<(), String> {
    let exe = std::env::current_exe()
        .and_then(std::fs::canonicalize)
        .map_err(|e| format!("cannot find this program: {e}"))?;
    let releases = Releases::from_env();
    let current = Version::current();
    let latest = releases
        .latest()
        .map_err(|e| format!("checking for updates: {e}"))?;
    let managed = Managed::of(&exe);

    if latest <= current && !args.force {
        println!("dfdu {current} is the latest release.");
        return Ok(());
    }
    if latest > current {
        println!("dfdu {latest} is out; this is {current}.");
        println!("Release notes: {}", releases.page(&latest));
    }
    if let Some(m) = managed {
        println!("This dfdu was {}", m.advice());
        return Ok(());
    }
    if args.check {
        println!("Run `dfdu update` to install it.");
        return Ok(());
    }
    // What an earlier update on Windows moved aside, no longer running.
    if cfg!(windows) {
        cli::remove_old(&exe);
    }
    install(&releases, &latest, &exe)?;
    println!("Updated {} to dfdu {latest}.", exe.display());
    Ok(())
}

/// Downloads release `v` for this machine and puts it at `exe`, with the extras already
/// installed beside it refreshed.
#[cfg(feature = "self-update")]
fn install(releases: &Releases, v: &Version, exe: &Path) -> Result<(), String> {
    let target = cli::target().ok_or("there is no dfdu release build for this platform")?;
    let asset = cli::asset(target);
    eprintln!("Downloading {asset}…");
    let archive = releases
        .fetch(v, &asset, cli::MAX_ARCHIVE)
        .map_err(|e| e.to_string())?;
    let files = cli::unpack(&archive, target).map_err(|e| format!("{asset}: {e}"))?;
    cli::install_bin(exe, &files.bin).map_err(|e| denied(exe, &e))?;
    if cfg!(unix)
        && let Some(dirs) = ExtraDirs::from_env()
    {
        let written = dirs.install(&files.extras, true);
        if !written.is_empty() {
            eprintln!(
                "Refreshed {} installed man pages and completions.",
                written.len()
            );
        }
    }
    Ok(())
}

/// Why `exe` could not be replaced, with what to do when it is a matter of permissions.
#[cfg(feature = "self-update")]
fn denied(exe: &Path, e: &std::io::Error) -> String {
    let dir = exe.parent().map_or_else(PathBuf::new, Path::to_path_buf);
    if e.kind() == std::io::ErrorKind::PermissionDenied {
        let how = if cfg!(windows) {
            "run it from an administrator's terminal"
        } else {
            "run: sudo dfdu update"
        };
        format!("cannot write to {}; {how}", dir.display())
    } else {
        format!("installing into {}: {e}", dir.display())
    }
}
