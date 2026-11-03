//! What the window opens, built as `dfdu` builds it ([`prepare`]): a scan read from a file
//! (`--import`), a directory on this host opened from its cached scan when there is one, or the
//! start screen; and the engine and session of what is opened ([`open`]).

use std::ffi::OsString;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use dfdu_engine::{Engine, EngineOptions};
use dfdu_rules::RuleSet;
use dfdu_scan::classify::ClassOpts;
use dfdu_scan::{JobOptions, NativeBackend, native_backend};
use dfdu_session::startup::{self, Opened, Source, StartLocal};

use crate::args::Args;
use crate::browser::{Browse, Browser};

/// How what the user opens is started.
pub struct Settings {
    pub engine: EngineOptions,
    /// Scans of this machine are recorded in the history.
    pub record: bool,
    /// Scans classify with the rules (no `--no-rules`): an administrator's scan sends the
    /// rule files to its agent.
    pub rules: bool,
}

/// What the window opens, and how.
pub struct Launch {
    /// A [`Source::Local`] without a path opens the start screen.
    pub source: Source,
    pub settings: Settings,
    /// Scan the path, or what the start screen chooses, as an administrator (`--admin`).
    pub admin: bool,
}

/// What `args` open, as `dfdu` opens them: an imported scan is browsed read-only and recorded
/// nowhere; a directory named is opened from its cached scan when there is one.
///
/// # Errors
///
/// The scan read from a file could not be read: the message says why.
pub fn prepare(args: &Args) -> Result<Launch, String> {
    let engine = EngineOptions {
        read_only: args.read_only,
        ..EngineOptions::from_env()
    };
    let settings = Settings {
        engine,
        record: args.import.is_none(),
        rules: !args.no_rules,
    };
    if let Some(file) = &args.import {
        let tree = import(file).map_err(|e| format!("{}: {e}", file.display()))?;
        return Ok(Launch {
            source: Source::Imported(tree),
            settings,
            admin: false,
        });
    }
    let rules = (!args.no_rules).then(load_rules);
    // An administrator's scan is never cached: it sees what the user's own scans do not.
    let slot = (args.path.as_deref().filter(|_| !args.admin)).and_then(cache_slot);
    let start = local(rules);
    let source = match (args.path.clone(), slot) {
        (Some(path), Some(slot)) => Source::Cached { path, slot, start },
        (path, _) => Source::Local { path, start },
    };
    Ok(Launch {
        source,
        settings,
        admin: args.admin,
    })
}

/// Whether `e` says the user dismissed the password dialog: the authorization sheet on macOS;
/// on Linux pkexec exits without the agent, which reads as refused or cancelled alike.
fn cancelled(e: &io::Error) -> bool {
    #[cfg(target_os = "macos")]
    return dfdu_sys::helper::is_cancelled(e);
    #[cfg(not(target_os = "macos"))]
    return e.kind() == io::ErrorKind::PermissionDenied;
}

/// Where the .deb installs the program.
#[cfg_attr(target_os = "macos", allow(dead_code))]
const DEB_EXE: &str = "/usr/bin/dfdu-gui";

/// The agent the .deb installs beside it: a script running `dfdu-gui --agent`, named by the
/// polkit action `app.dfdu.agent`, so pkexec asks for the password in dfdu's name.
#[cfg_attr(target_os = "macos", allow(dead_code))]
const DEB_AGENT: &str = "/usr/libexec/dfdu/dfdu-agent";

/// The agent of an administrator's scan: this program with `--agent`; inside an AppImage the
/// AppImage file itself, since its mount is not readable by root; installed from the .deb, its
/// agent script.
#[cfg_attr(target_os = "macos", allow(dead_code))]
fn agent_command() -> io::Result<Vec<OsString>> {
    // Only an AppImage that is a file: pkexec runs it as root.
    let appimage = std::env::var_os("APPIMAGE")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute() && std::fs::metadata(p).is_ok_and(|m| m.is_file()));
    // The AppDir as it is on disk, as the running program's path is; only one with the AppRun
    // an AppImage starts from, so not `/` or `/usr` around an installed program.
    let appdir = std::env::var_os("APPDIR")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .and_then(|p| std::fs::canonicalize(p).ok())
        .filter(|p| p.join("AppRun").is_file());
    Ok(agent_argv(
        appimage,
        appdir,
        std::env::current_exe()?,
        Path::new(DEB_AGENT).is_file(),
    ))
}

/// The agent's command, from `$APPIMAGE` (`appimage`), `$APPDIR` (`appdir`), the running program
/// (`exe`) and whether the .deb's agent script is installed (`deb_agent`): the script only for
/// the program the .deb installed, since it runs that one, not some other build started beside
/// it.
///
/// The environment is the user's, so anything running as the user can set `APPIMAGE`, and
/// pkexec would run what it names as root once the password is given. It is taken only when this
/// program really runs from that AppImage: from inside `$APPDIR` (the AppImage's mount, set by its
/// runtime), and never over the .deb's script.
#[cfg_attr(target_os = "macos", allow(dead_code))]
fn agent_argv(
    appimage: Option<PathBuf>,
    appdir: Option<PathBuf>,
    exe: PathBuf,
    deb_agent: bool,
) -> Vec<OsString> {
    if deb_agent && exe == Path::new(DEB_EXE) {
        return vec![DEB_AGENT.into()];
    }
    let inside = |dir: &Path| dir.is_absolute() && exe.starts_with(dir) && exe != dir;
    if let Some(appimage) = appimage.filter(|p| p.is_absolute())
        && appdir.as_deref().is_some_and(inside)
    {
        return vec![appimage.into_os_string(), "--agent".into()];
    }
    vec![exe.into_os_string(), "--agent".into()]
}

/// What an administrator's scan opened, or why not: `Err(None)` when the password dialog was
/// cancelled (the start screen shows again), otherwise the reason.
pub type Elevating = Result<dfdu_session::elevate::Elevated, Option<String>>;

/// Starts an administrator's scan of `path` on a thread of its own, since the system's
/// password dialog keeps the agent from answering until it is done; the source it opens, or why
/// it cannot, comes back on the channel, with a frame asked of `ctx`.
pub fn elevate(
    path: PathBuf,
    settings: &Settings,
    ctx: &egui::Context,
) -> crossbeam_channel::Receiver<Elevating> {
    let (tx, rx) = crossbeam_channel::bounded(1);
    let rules = settings.rules.then(|| {
        let (files, errors) = dfdu_rules::sources(dfdu_sys::dirs::rules_dir().as_deref());
        for e in errors {
            eprintln!("dfdu-gui: {e}");
        }
        dfdu_engine::RemoteRules { files }
    });
    let opts = settings.engine.clone();
    let ctx = ctx.clone();
    let connect = move || {
        // macOS: the helper in the app bundle; elsewhere through pkexec.
        #[cfg(target_os = "macos")]
        let io = crate::helper::connect();
        #[cfg(not(target_os = "macos"))]
        let io = agent_command().and_then(|agent| dfdu_session::elevate::spawn(&agent));
        let opened = io
            .and_then(|io| {
                dfdu_session::elevate::connect(io, &path, rules, opts, JobOptions::default())
            })
            .map(|mut e| {
                for msg in std::mem::take(&mut e.errors) {
                    eprintln!("dfdu-gui: {msg}");
                }
                e
            })
            .map_err(|e| {
                (!cancelled(&e))
                    .then(|| format!("Cannot scan {} as an administrator: {e}", path.display()))
            });
        let _ = tx.send(opened);
        ctx.request_repaint();
    };
    if let Err(e) = std::thread::Builder::new()
        .name("dfdu-gui-elevate".into())
        .spawn(connect)
    {
        let (tx, rx) = crossbeam_channel::bounded(1);
        let _ = tx.send(Err(Some(format!("cannot start a thread: {e}"))));
        return rx;
    }
    rx
}

/// Runs as the agent of an administrator's scan (`--agent`, started by [`elevate`] through
/// pkexec): acts for the user who started it, scans with this host's backend, journals deletes
/// in their state directory, and speaks the wire protocol on standard input and output, where
/// nothing else may be written.
pub fn agent() -> std::process::ExitCode {
    // First: may execute this program again with the user's HOME.
    if let Err(e) = dfdu_sys::acting::adopt() {
        eprintln!("dfdu-gui --agent: {e}");
        return std::process::ExitCode::from(1);
    }
    dfdu_sys::dirs::give_back();
    let storage = dfdu_sys::Storage::Unknown;
    let r = dfdu_engine::agent::run(
        native_backend(storage, false),
        dfdu_engine::workers_for(storage),
        dfdu_sys::dirs::journal_dir(),
        io::stdin(),
        io::stdout(),
    );
    dfdu_sys::dirs::give_back();
    match r {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("dfdu-gui --agent: {e}");
            std::process::ExitCode::from(1)
        }
    }
}

/// Starts the engine of `source` and the session that browses it, which asks `ctx` for a frame
/// when the engine's model changes.
///
/// # Errors
///
/// As [`startup::open`].
pub fn open(
    source: Source,
    settings: &Settings,
    ctx: &egui::Context,
) -> io::Result<Box<dyn Browse>> {
    Ok(
        match startup::open(source, &settings.engine, settings.record)? {
            Opened::Native(s) => Box::new(Browser::new(s, ctx)),
            Opened::Imported(s) => Box::new(Browser::new(s, ctx)),
        },
    )
}

/// The source the start screen opens for `path`: scanned afresh, as a disk chosen in `dfdu`'s
/// picker is.
pub fn picked(path: PathBuf, start: StartLocal) -> Source {
    Source::Local {
        path: Some(path),
        start,
    }
}

/// Starts engines on this host's files as `dfdu` does: the native backend for the storage the
/// directory is on, with as many workers as suit it, classifying with `rules`. Mounts are not
/// entered, and NTFS volumes are listed directory by directory.
pub fn local(rules: Option<Arc<RuleSet>>) -> StartLocal {
    Box::new(move |path, opts| start(path, opts, rules))
}

fn start(
    path: &Path,
    opts: EngineOptions,
    rules: Option<Arc<RuleSet>>,
) -> (Engine<NativeBackend>, JobOptions) {
    let storage = dfdu_sys::storage(path);
    let opts = EngineOptions {
        workers: dfdu_engine::workers_for(storage),
        ..opts
    };
    let engine = Engine::start(native_backend(storage, false), opts);
    let job = JobOptions {
        class: rules.map(|r| ClassOpts::for_root(r, path)),
        ..JobOptions::default()
    };
    (engine, job)
}

/// The built-in rules with the user's rule files. A rule that does not load is reported and
/// left out.
fn load_rules() -> Arc<RuleSet> {
    let dir = dfdu_sys::dirs::rules_dir();
    let (set, errors) = dfdu_rules::load(&dfdu_rules::Env::current(), dir.as_deref());
    for e in errors {
        eprintln!("dfdu-gui: {e}");
    }
    Arc::new(set)
}

/// Where the scan of `path` is cached; `None` when the cache is off, refused (said on standard
/// error), or `path` is not a directory (the scan then says why).
fn cache_slot(path: &Path) -> Option<dfdu_store::cache::Slot> {
    let cache = match dfdu_store::cache::Cache::open_default()? {
        Ok(cache) => cache,
        Err(e) => {
            eprintln!("dfdu-gui: {e}; the scan cache is not used");
            return None;
        }
    };
    let key = dfdu_store::cache::Key::new(path, &JobOptions::default(), false).ok()?;
    Some(cache.slot(key))
}

/// The tree of a scan saved in `file`: a snapshot, or an ncdu export.
///
/// # Errors
///
/// The file cannot be read or is not a scan.
pub fn import(file: &Path) -> io::Result<dfdu_tree::Tree> {
    let f = std::fs::File::open(file)?;
    let opened = dfdu_store::open_file(f, &dfdu_store::Limits::default())
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e.to_string()))?;
    Ok(opened.tree)
}

// The paths are Unix ones (an administrator's scan goes through pkexec, on Linux only).
#[cfg(all(test, unix))]
mod tests {
    use super::*;

    /// Where an AppImage's runtime mounts it, and the program inside.
    const APPDIR: &str = "/tmp/.mount_dfdu";
    const MOUNTED: &str = "/tmp/.mount_dfdu/usr/bin/dfdu-gui";

    /// With `$APPDIR` set to [`APPDIR`] whenever `$APPIMAGE` is.
    fn argv(appimage: Option<&str>, exe: &str, deb_agent: bool) -> Vec<OsString> {
        let appdir = appimage.map(|_| PathBuf::from(APPDIR));
        agent_argv(
            appimage.map(PathBuf::from),
            appdir,
            PathBuf::from(exe),
            deb_agent,
        )
    }

    /// Given `$APPIMAGE` of something running as the user.
    fn planted(appdir: Option<&str>, exe: &str, deb_agent: bool) -> Vec<OsString> {
        agent_argv(
            Some("/home/u/evil".into()),
            appdir.map(PathBuf::from),
            exe.into(),
            deb_agent,
        )
    }

    #[test]
    fn agent_is_this_program_by_default() {
        assert_eq!(
            argv(None, "/home/u/dfdu-gui", false),
            ["/home/u/dfdu-gui", "--agent"]
        );
    }

    #[test]
    fn agent_is_the_appimage_inside_one() {
        let want = ["/home/u/dfdu.AppImage", "--agent"];
        assert_eq!(argv(Some("/home/u/dfdu.AppImage"), MOUNTED, false), want);
        // The .deb installed as well changes nothing: its script runs its own program.
        assert_eq!(argv(Some("/home/u/dfdu.AppImage"), MOUNTED, true), want);
    }

    #[test]
    fn relative_appimage_is_ignored() {
        assert_eq!(
            argv(Some("dfdu.AppImage"), MOUNTED, false),
            [MOUNTED, "--agent"]
        );
    }

    #[test]
    fn deb_script_wins_over_an_appimage() {
        assert_eq!(planted(None, DEB_EXE, true), [DEB_AGENT]);
        assert_eq!(planted(Some("/usr"), DEB_EXE, true), [DEB_AGENT]);
    }

    #[test]
    fn appimage_is_refused_unless_this_program_runs_inside_its_appdir() {
        // No APPDIR: not started by an AppImage's runtime.
        assert_eq!(planted(None, MOUNTED, false), [MOUNTED, "--agent"]);
        // This program is outside the APPDIR given.
        let built = "/home/u/target/release/dfdu-gui";
        assert_eq!(planted(Some(APPDIR), built, false), [built, "--agent"]);
        let near = "/tmp/.mount_dfd";
        assert_eq!(planted(Some(near), MOUNTED, false), [MOUNTED, "--agent"]);
        // A relative APPDIR, or the program itself given as the APPDIR.
        let relative = "tmp/.mount_dfdu";
        assert_eq!(
            planted(Some(relative), MOUNTED, false),
            [MOUNTED, "--agent"]
        );
        assert_eq!(planted(Some(MOUNTED), MOUNTED, false), [MOUNTED, "--agent"]);
    }

    #[test]
    fn appimage_is_taken_when_this_program_runs_inside_its_appdir() {
        assert_eq!(
            planted(Some(APPDIR), MOUNTED, false),
            ["/home/u/evil", "--agent"]
        );
    }

    #[test]
    fn agent_is_the_deb_script_when_installed_from_the_deb() {
        assert_eq!(argv(None, DEB_EXE, true), [DEB_AGENT]);
    }

    #[test]
    fn deb_script_is_only_for_the_deb_program() {
        assert_eq!(argv(None, DEB_EXE, false), [DEB_EXE, "--agent"]);
        assert_eq!(
            argv(None, "/home/u/target/release/dfdu-gui", true),
            ["/home/u/target/release/dfdu-gui", "--agent"]
        );
    }
}
