//! macOS: the helper that runs scans as an administrator. It is this program, started by
//! launchd as root with `--helper` from the launch daemon in the app bundle
//! (`Contents/Library/LaunchDaemons/app.dfdu.helper.plist`, registered with SMAppService from
//! the Administrator access window, `crate::admin`).
//!
//! For each scan the window makes a socket pair and sends one end to the helper over XPC
//! ([`connect`]); the helper checks that the window is signed by dfdu's team as `app.dfdu`,
//! and runs `--agent` as root on that socket, acting for the window's user
//! (`DFDU_ACTING_UID`). The window checks the helper the same way before sending anything. The
//! helper exits a minute after its last agent ends; launchd starts it again for the next scan.

use std::io;
use std::os::fd::AsFd;
use std::os::unix::net::UnixStream;
use std::process::{Command, ExitCode, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use dfdu_engine::RemoteIo;

/// The launch daemon's plist in the app bundle, as SMAppService names it.
pub const PLIST: &str = "app.dfdu.helper.plist";
/// The helper's mach service (the plist's `MachServices`).
pub const SERVICE: &str = "app.dfdu.helper";
/// The code signature both sides require of the other: dfdu's app, signed by dfdu's team
/// (Developer ID for releases, Apple Development for local builds; both are Apple-anchored and
/// name the team in the leaf's organizational unit).
pub const REQUIREMENT: &str = r#"anchor apple generic and identifier "app.dfdu" and certificate leaf[subject.OU] = "7YZ4RG7YHP""#;

/// How long the helper stays after its last agent, before launchd has to start it again.
const IDLE: Duration = Duration::from_secs(60);
/// How long the window waits for the helper to answer.
const ANSWER: Duration = Duration::from_secs(15);

/// Connects to the helper and returns the link its agent will speak over.
///
/// # Errors
///
/// When the helper is not installed or approved, its signature does not match, or it does not
/// answer: the message says which.
pub fn connect() -> io::Result<RemoteIo> {
    let (ours, theirs) = UnixStream::pair()?;
    dfdu_sys::helper::connect(SERVICE, REQUIREMENT, theirs.as_fd(), ANSWER)?;
    // The helper has its own copy of `theirs` now.
    drop(theirs);
    let reader = io::BufReader::new(ours.try_clone()?);
    Ok(RemoteIo {
        reader: Box::new(reader),
        writer: Box::new(ours),
        child: None,
    })
}

/// Runs as the helper (`--helper`): answers the windows that meet [`REQUIREMENT`], each with
/// an agent of its own, until a minute passes with no agent running.
pub fn serve() -> ExitCode {
    let exe = match std::env::current_exe() {
        Ok(e) => e,
        Err(e) => {
            eprintln!("dfdu helper: {e}");
            return ExitCode::from(1);
        }
    };
    let running = Arc::new(AtomicUsize::new(0));
    let idle = Arc::new(std::sync::Mutex::new(Instant::now()));
    {
        let (running, idle) = (Arc::clone(&running), Arc::clone(&idle));
        std::thread::spawn(move || {
            loop {
                std::thread::sleep(Duration::from_secs(5));
                let since = idle.lock().map_or(Duration::ZERO, |t| t.elapsed());
                if running.load(Ordering::SeqCst) == 0 && since >= IDLE {
                    std::process::exit(0);
                }
            }
        });
    }
    let on_peer = move |peer: dfdu_sys::helper::Peer| {
        // Only an administrator may scan and delete as root, as with sudo: the app can be run
        // by every user of the Mac once an administrator approved the helper. Closing the link
        // tells the window it was refused.
        if !may_use(peer.euid) {
            eprintln!("dfdu helper: uid {} is not an administrator", peer.euid);
            return;
        }
        let started = (|| {
            let output = peer.link.try_clone()?;
            Command::new(&exe)
                .arg("--agent")
                .env_clear()
                .env(dfdu_sys::acting::ACTING_UID_VAR, peer.euid.to_string())
                // System directories only: they are root's. Native commands run as the user
                // and look in Homebrew's as well (`dfdu_sys::acting::run_as`).
                .env("PATH", "/usr/bin:/bin:/usr/sbin:/sbin")
                .stdin(Stdio::from(peer.link))
                .stdout(Stdio::from(output))
                .stderr(Stdio::null())
                .spawn()
        })();
        match started {
            Ok(mut child) => {
                running.fetch_add(1, Ordering::SeqCst);
                let (running, idle) = (Arc::clone(&running), Arc::clone(&idle));
                std::thread::spawn(move || {
                    let _ = child.wait();
                    if let Ok(mut t) = idle.lock() {
                        *t = Instant::now();
                    }
                    running.fetch_sub(1, Ordering::SeqCst);
                });
            }
            Err(e) => eprintln!("dfdu helper: cannot start the agent: {e}"),
        }
    };
    let e = dfdu_sys::helper::serve(SERVICE, REQUIREMENT, on_peer);
    eprintln!("dfdu helper: {e}");
    ExitCode::from(1)
}

/// Whether the user `uid` may use the helper: a member of the `admin` group, other than root.
pub fn may_use(uid: u32) -> bool {
    uid != 0
        && dfdu_sys::acting::user(uid)
            .is_some_and(|u| u.groups.contains(&dfdu_sys::acting::ADMIN_GID))
}

/// Whether this app can read what macOS keeps from apps without Full Disk Access: the helper
/// shares the app's permission, so this answers for it too. Run from a terminal, the window
/// has the terminal's permission instead, so it may answer yes when the helper's is no.
pub fn full_disk_access() -> bool {
    std::fs::read_dir("/Library/Application Support/com.apple.TCC").is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn root_and_unknown_users_may_not_use_the_helper() {
        assert!(!may_use(0));
        assert!(!may_use(4_000_000_000), "no such user");
        // `nobody` is never an administrator.
        assert!(!may_use(4_294_967_294));
    }

    #[test]
    fn administrators_may() {
        let me = dfdu_sys::acting::this_uid();
        let admin = dfdu_sys::acting::user(me)
            .is_some_and(|u| u.groups.contains(&dfdu_sys::acting::ADMIN_GID));
        assert_eq!(may_use(me), admin && me != 0);
    }
}
