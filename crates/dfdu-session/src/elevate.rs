//! Scans of this machine as root while the front end runs as its user: an agent
//! (`dfdu agent`, `dfdu-gui --agent`) started through an elevation program, connected over its
//! standard streams as a remote agent is over ssh. The agent scans, deletes (journalled in the
//! user's state directory) and runs native commands (as the user) on its side; it acts for the
//! user who started it (`dfdu_sys::acting`). The tree is of this machine
//! ([`Source::Remote`] with `local`), so files open in the file manager, mounts outside the
//! scan are looked up here and duplicates are compared. Nothing is recorded in the history:
//! a root scan sees what the user's own scans cannot, so the two do not compare.

use std::ffi::OsString;
use std::io::{self, BufReader};
use std::path::Path;
use std::process::{Command, Stdio};

use dfdu_engine::{Engine, EngineOptions, RemoteIo, RemoteRules};
use dfdu_scan::JobOptions;

use crate::startup::Source;

/// The environment variable that replaces the elevation command (split at spaces), which the
/// agent's program and arguments follow. Empty runs the agent without elevation (tests).
pub const ELEVATE_VAR: &str = "DFDU_ELEVATE_COMMAND";

/// How this system asks for an administrator to run a program: `pkexec` on Linux (polkit's
/// dialog in the desktop session, which keeps the program's standard streams). `None` where
/// the front end has no way (macOS uses a launch daemon, Windows is not supported).
#[must_use]
pub fn elevator() -> Option<Vec<OsString>> {
    if let Some(c) = std::env::var_os(ELEVATE_VAR) {
        return Some(
            c.to_string_lossy()
                .split_whitespace()
                .map(OsString::from)
                .collect(),
        );
    }
    if cfg!(target_os = "linux") {
        Some(vec!["pkexec".into()])
    } else {
        None
    }
}

/// Starts `agent` (a program and its arguments, the program an absolute path, as pkexec
/// requires) through [`elevator`], its standard input and output piped as the link and its
/// errors shown where the front end's are.
///
/// # Errors
///
/// When the system has no elevation program or it cannot be started.
pub fn spawn(agent: &[OsString]) -> io::Result<RemoteIo> {
    let mut argv = elevator().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::Unsupported,
            "this system has no way to scan as an administrator here",
        )
    })?;
    argv.extend(agent.iter().cloned());
    let (prog, rest) = argv
        .split_first()
        .ok_or_else(|| io::Error::other("the agent command is empty"))?;
    let mut child = Command::new(prog)
        .args(rest)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .map_err(|e| io::Error::new(e.kind(), format!("{}: {e}", prog.to_string_lossy())))?;
    let pipe = |what| io::Error::other(format!("the agent has no {what} pipe"));
    let reader = BufReader::new(child.stdout.take().ok_or_else(|| pipe("output"))?);
    let writer = child.stdin.take().ok_or_else(|| pipe("input"))?;
    Ok(RemoteIo {
        reader: Box::new(reader),
        writer: Box::new(writer),
        child: Some(child.into()),
    })
}

/// What [`connect`] opened, and what the agent reported on the way. It can be sent to the
/// thread that opens it ([`Elevated::source`]), unlike a [`Source`].
pub struct Elevated {
    pub engine: Engine<dfdu_scan::NativeBackend>,
    /// The root, as the agent resolved it.
    pub path: dfdu_tree::HostPath,
    pub job: JobOptions,
    /// Problems with the rules, as the agent found them.
    pub errors: Vec<String>,
    /// Whether the agent runs as root (an elevation that was refused never gets here; one that
    /// runs the agent as the user does, `false`).
    pub privileged: bool,
}

/// Connects to the agent of `io` and has it scan `root` (as typed: the agent resolves it),
/// classifying with `rules`, with `job`'s other options.
///
/// # Errors
///
/// When the agent does not answer: elevation refused or cancelled (pkexec exits 126 or 127
/// before the agent starts), or it speaks no common protocol version.
pub fn connect(
    io: RemoteIo,
    root: &Path,
    rules: Option<RemoteRules>,
    opts: EngineOptions,
    job: JobOptions,
) -> io::Result<Elevated> {
    let backend = dfdu_scan::native_backend(dfdu_sys::Storage::Unknown, false);
    let (engine, c) = Engine::start_remote(backend, opts, io, root, rules).map_err(|e| {
        if e.kind() == io::ErrorKind::UnexpectedEof {
            io::Error::new(
                io::ErrorKind::PermissionDenied,
                "authorization was refused or cancelled",
            )
        } else {
            e
        }
    })?;
    Ok(Elevated {
        engine,
        path: c.root,
        job: JobOptions {
            class: c.class,
            ..job
        },
        errors: c.errors,
        privileged: c.privileged,
    })
}

impl Elevated {
    /// The source to open: the agent's tree, of this machine.
    #[must_use]
    pub fn source(self) -> Source {
        Source::Remote {
            engine: self.engine,
            path: self.path,
            job: self.job,
            local: true,
            elevated: self.privileged,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::startup::{Opened, open};

    /// An agent on a thread of this process, as `dfdu agent` would run under pkexec.
    fn agent() -> (RemoteIo, std::thread::JoinHandle<()>) {
        let (down_r, down_w) = std::io::pipe().unwrap();
        let (up_r, up_w) = std::io::pipe().unwrap();
        let journal = tempfile::tempdir().unwrap().keep();
        let t = std::thread::spawn(move || {
            let backend = dfdu_scan::native_backend(dfdu_sys::Storage::Unknown, false);
            let _ = dfdu_engine::agent::run(backend, 2, Some(journal), down_r, up_w);
        });
        let io = RemoteIo {
            reader: Box::new(BufReader::new(up_r)),
            writer: Box::new(down_w),
            child: None,
        };
        (io, t)
    }

    #[test]
    fn an_elevated_scan_is_of_this_machine() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("a")).unwrap();
        std::fs::write(dir.path().join("a/f"), vec![1u8; 10_000]).unwrap();
        let (io, agent) = agent();
        let opts = EngineOptions::default();
        let mut e = connect(io, dir.path(), None, opts.clone(), JobOptions::default()).unwrap();
        // The agent runs as this test does; as the helper or pkexec starts it, it says root.
        e.privileged = true;
        let source = e.source();
        let Source::Remote {
            path,
            local,
            elevated,
            ..
        } = &source
        else {
            panic!("not an agent's source");
        };
        assert!(*local && *elevated);
        assert_eq!(
            path.to_local(),
            dunce::canonicalize(dir.path()).unwrap(),
            "the agent resolved the root"
        );
        let Ok(Opened::Native(started)) = open(source, &opts, true) else {
            panic!("not opened");
        };
        let setup = &started.setup;
        assert!(
            setup.local,
            "this machine's files: file manager, mounts, duplicates"
        );
        assert_eq!(setup.home, dfdu_engine::home_dir());
        assert!(setup.jobs.history.is_none(), "not recorded in the history");
        assert!(
            setup.elevated,
            "a folder outside it is not looked up as this user"
        );
        started.engine.shutdown(std::time::Duration::from_secs(1));
        agent.join().unwrap();
    }

    #[test]
    fn an_agent_that_runs_as_the_user_is_not_elevated() {
        let dir = tempfile::tempdir().unwrap();
        let (io, agent) = agent();
        let opts = EngineOptions::default();
        let mut e = connect(io, dir.path(), None, opts.clone(), JobOptions::default()).unwrap();
        // An elevation program that ran the agent as the user.
        e.privileged = false;
        let Ok(Opened::Native(started)) = open(e.source(), &opts, true) else {
            panic!("not opened");
        };
        assert!(started.setup.local);
        assert!(
            !started.setup.elevated,
            "a folder outside it is looked up as this user"
        );
        started.engine.shutdown(std::time::Duration::from_secs(1));
        agent.join().unwrap();
    }

    #[test]
    fn an_agent_that_never_starts_is_a_refusal() {
        let (down_r, down_w) = std::io::pipe().unwrap();
        let (up_r, up_w) = std::io::pipe().unwrap();
        // pkexec exits before starting the agent when the dialog is cancelled.
        drop((down_r, up_w));
        let io = RemoteIo {
            reader: Box::new(up_r),
            writer: Box::new(down_w),
            child: None,
        };
        let err = connect(
            io,
            Path::new("/"),
            None,
            EngineOptions::default(),
            JobOptions::default(),
        )
        .err()
        .expect("refused");
        assert_eq!(err.kind(), io::ErrorKind::PermissionDenied, "{err}");
    }
}
