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

/// Starts the engine of `source`, keeping what `scanned` read below it
/// ([`startup::open_reusing`]), and the session that browses it, which asks `ctx` for a frame
/// when the engine's model changes.
///
/// # Errors
///
/// As [`startup::open`].
pub fn open(
    source: Source,
    scanned: Option<dfdu_engine::Scanned>,
    settings: &Settings,
    ctx: &egui::Context,
) -> io::Result<Box<dyn Browse>> {
    Ok(
        match startup::open_reusing(source, &settings.engine, settings.record, scanned)? {
            Opened::Native(s) => Box::new(Browser::new(s, ctx)),
            Opened::Imported(s) => {
                let mut b = Browser::new(s, ctx);
                b.state_mut().from_file = Some(crate::goto::FROM_FILE);
                Box::new(b)
            }
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

/// The source of a scan of `path` on this machine asked for from a browser (up past its root,
/// or a folder gone to), as `dfdu PATH` opens it: from its cached scan when there is one, with
/// the rules unless they are off.
pub fn reopened(path: PathBuf, settings: &Settings) -> Source {
    reopened_in(path, settings, dfdu_store::cache::Cache::open_default())
}

/// [`reopened`] with `cache`: `None` when it is off.
fn reopened_in(
    path: PathBuf,
    settings: &Settings,
    cache: Option<io::Result<dfdu_store::cache::Cache>>,
) -> Source {
    let start = local(settings.rules.then(load_rules));
    match slot_in(cache, &path) {
        Some(slot) => Source::Cached { path, slot, start },
        None => Source::Local {
            path: Some(path),
            start,
        },
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
    slot_in(dfdu_store::cache::Cache::open_default(), path)
}

/// [`cache_slot`] in `cache`: `None` when it is off.
fn slot_in(
    cache: Option<io::Result<dfdu_store::cache::Cache>>,
    path: &Path,
) -> Option<dfdu_store::cache::Slot> {
    let cache = match cache? {
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
    fn a_folder_asked_for_from_a_browser_opens_from_its_cached_scan_when_there_is_a_cache() {
        let tmp = tempfile::tempdir().expect("temp dir");
        let folder = tmp.path().join("disk1");
        std::fs::create_dir(&folder).expect("mkdir");
        let settings = Settings {
            engine: EngineOptions::default(),
            record: false,
            rules: false,
        };
        let cache = dfdu_store::cache::Cache::at(tmp.path().join("cache"));
        match reopened_in(folder.clone(), &settings, Some(cache)) {
            Source::Cached { path, .. } => assert_eq!(path, folder),
            _ => panic!("not from the cache"),
        }
        match reopened_in(folder.clone(), &settings, None) {
            Source::Local { path, .. } => assert_eq!(path, Some(folder.clone())),
            _ => panic!("not scanned afresh"),
        }
        // A cache that may not be used is left out, and the folder scanned afresh.
        let refused = Err(io::Error::from(io::ErrorKind::PermissionDenied));
        match reopened_in(folder.clone(), &settings, Some(refused)) {
            Source::Local { path, .. } => assert_eq!(path, Some(folder)),
            _ => panic!("not scanned afresh"),
        }
    }

    /// The browser of an administrator's scan, from the helper's agent on macOS or pkexec's
    /// elsewhere (here on a thread of this process, reporting itself as `privileged`).
    fn elevated_browser(privileged: bool) -> Box<dyn Browse> {
        let dir = tempfile::tempdir().expect("temp dir");
        let (down_r, down_w) = std::io::pipe().expect("pipe");
        let (up_r, up_w) = std::io::pipe().expect("pipe");
        let journal = tempfile::tempdir().expect("temp dir").keep();
        std::thread::spawn(move || {
            let backend = native_backend(dfdu_sys::Storage::Unknown, false);
            let _ = dfdu_engine::agent::run(backend, 1, Some(journal), down_r, up_w);
        });
        let io = dfdu_engine::RemoteIo {
            reader: Box::new(io::BufReader::new(up_r)),
            writer: Box::new(down_w),
            child: None,
        };
        let opts = EngineOptions::default();
        let job = JobOptions::default();
        let mut e = dfdu_session::elevate::connect(io, dir.path(), None, opts.clone(), job)
            .expect("connected");
        e.privileged = privileged;
        let settings = Settings {
            engine: opts,
            record: false,
            rules: false,
        };
        open(e.source(), None, &settings, &egui::Context::default()).expect("opened")
    }

    #[test]
    fn an_administrators_scan_does_not_look_up_a_folder_as_this_user() {
        let b = elevated_browser(true);
        assert!(b.state().outside.local && b.state().elevated);
        let b = elevated_browser(false);
        let s = b.state();
        assert!(s.outside.local && !s.elevated, "the agent runs as the user");
    }

    /// A browser of this machine's scan offers what it read to a scan of a folder above its
    /// root once it ended; an administrator's, read by an agent, does not. On Unix, where the
    /// canonical path the scan names its root by is what `canonicalize` gives.
    #[cfg(unix)]
    #[test]
    fn a_local_browser_offers_what_it_read_to_a_folder_above() {
        let tmp = tempfile::tempdir().expect("temp dir");
        let top = std::fs::canonicalize(tmp.path()).expect("canonical");
        let dir = top.join("c");
        std::fs::create_dir_all(dir.join("d")).expect("mkdir");
        let settings = Settings {
            engine: EngineOptions::default(),
            record: false,
            rules: false,
        };
        let source = picked(dir.clone(), local(None));
        let b = open(source, None, &settings, &egui::Context::default()).expect("opened");
        let above = dfdu_tree::HostPath::local(&top);
        let t0 = std::time::Instant::now();
        let scanned = loop {
            if let Some(s) = b.scanned(&above) {
                break s;
            }
            assert!(
                t0.elapsed() < std::time::Duration::from_secs(10),
                "timed out"
            );
            std::thread::sleep(std::time::Duration::from_millis(5));
        };
        assert_eq!(scanned.root(), dfdu_tree::HostPath::local(&dir));
        assert!(
            b.scanned(&dfdu_tree::HostPath::local(&dir)).is_none(),
            "not above its root"
        );
        b.shutdown();
        let b = elevated_browser(false);
        assert!(b.scanned(&above).is_none());
        b.shutdown();
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
