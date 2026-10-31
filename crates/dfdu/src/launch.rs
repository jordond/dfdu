//! Starting engines on this host's files, for the headless commands and the browser alike, and
//! running a headless command's scan to the end.

use std::path::Path;
use std::process::ExitCode;
use std::sync::Arc;
use std::time::{Duration, Instant};

use dfdu_engine::{Command, Engine, EngineOptions, Model, WaitIdle};
use dfdu_rules::RuleSet;
use dfdu_scan::classify::ClassOpts;
use dfdu_scan::{Backend, JobOptions, NativeBackend, native_backend};
use dfdu_sys::Storage;

use crate::MountArgs;

/// How long a headless scan waits on a stalled directory before abandoning it.
pub const STALL_ABANDON: Duration = Duration::from_secs(30);
/// How long a headless command waits for the engine's threads to stop.
pub const SHUTDOWN: Duration = Duration::from_millis(500);

/// Options of an engine for a headless command: stalled directories are abandoned after
/// [`STALL_ABANDON`]. It can delete, journalled in dfdu's state directory, only with `journal`.
pub fn headless(journal: bool) -> EngineOptions {
    EngineOptions {
        auto_abandon: Some(STALL_ABANDON),
        read_only: !journal,
        journal_dir: if journal {
            dfdu_sys::dirs::journal_dir()
        } else {
            None
        },
        ..EngineOptions::from_env()
    }
}

/// `opts` with `threads` workers, or as many as suit the storage `path` is on, and that
/// storage.
pub fn for_storage(
    path: &Path,
    threads: Option<usize>,
    opts: EngineOptions,
) -> (Storage, EngineOptions) {
    let storage = dfdu_sys::storage(path);
    let opts = EngineOptions {
        workers: threads.unwrap_or_else(|| dfdu_engine::workers_for(storage)),
        ..opts
    };
    (storage, opts)
}

/// How a scan of this host's files is set up: the native backend for the storage the scanned
/// directory is on, with as many workers as suit it, classifying with the rules. Each command
/// fills in the fields itself: they differ in when --mft applies and which mounts are entered.
pub struct Local {
    /// List NTFS volumes from their master file tables ([`MountArgs::use_mft`]).
    pub mft: bool,
    /// Worker threads; `None` chooses them for the storage.
    pub threads: Option<usize>,
    /// Which mounts the scan descends into.
    pub job: JobOptions,
    /// The rules entries are classified against; `None` classifies nothing.
    pub rules: Option<Arc<RuleSet>>,
}

impl Local {
    /// `opts` with this scan's workers for the storage `path` is on ([`for_storage`]), and
    /// that storage.
    pub fn engine_options(&self, path: &Path, opts: EngineOptions) -> (Storage, EngineOptions) {
        for_storage(path, self.threads, opts)
    }

    /// The native backend for data on `storage`.
    pub fn backend(&self, storage: Storage) -> NativeBackend {
        native_backend(storage, self.mft)
    }

    /// The options of the scan of `path`, which classifies against the root `path`.
    pub fn job(&self, path: &Path) -> JobOptions {
        JobOptions {
            class: self.rules.clone().map(|r| ClassOpts::for_root(r, path)),
            ..self.job.clone()
        }
    }

    /// An engine with `opts` for a scan of `path`, and the options of that scan. Nothing is
    /// sent to it yet.
    pub fn start(self, path: &Path, opts: EngineOptions) -> (Engine<NativeBackend>, JobOptions) {
        let (storage, opts) = self.engine_options(path, opts);
        let engine = Engine::start(self.backend(storage), opts);
        (engine, self.job(path))
    }
}

/// Starts a headless engine ([`headless`]) on `path` and runs its scan to the end
/// ([`run_scan`]).
pub fn scan(
    path: &Path,
    mounts: MountArgs,
    threads: Option<usize>,
    rules: Option<Arc<RuleSet>>,
    journal: bool,
) -> Result<Engine<NativeBackend>, ExitCode> {
    let local = Local {
        mft: mounts.use_mft(path),
        threads,
        job: mounts.job(),
        rules,
    };
    let (engine, job) = local.start(path, headless(journal));
    run_scan(
        engine,
        Command::Scan {
            path: path.to_path_buf().into(),
            opts: job,
        },
    )
}

/// Sends `cmd` (a scan) and waits until the engine is done with it ([`wait_scan`]).
pub fn run_scan<B: Backend>(engine: Engine<B>, cmd: Command) -> Result<Engine<B>, ExitCode> {
    engine.send(cmd);
    wait_scan(engine)
}

/// Waits until the engine is done with the scan or resume sent to it. An engine that failed,
/// or a link to the agent that ended before anything arrived, ends the command: the reason is
/// reported, the engine stopped and exit code 1 returned. A link that ended later is reported
/// and leaves an incomplete tree to print.
pub fn wait_scan<B: Backend>(engine: Engine<B>) -> Result<Engine<B>, ExitCode> {
    if let WaitIdle::Fatal(msg) = engine.wait_idle(None) {
        eprintln!("dfdu: {msg}");
        engine.shutdown(SHUTDOWN);
        return Err(ExitCode::from(1));
    }
    let (gone, empty) = engine.read(|m| {
        (
            m.status.disconnected.clone(),
            m.tree.roots().next().is_none(),
        )
    });
    if let Some(why) = gone {
        eprintln!("dfdu: remote scan: {why}");
        if empty {
            engine.shutdown(SHUTDOWN);
            return Err(ExitCode::from(1));
        }
    }
    Ok(engine)
}

/// Reads the model with `f` every 10 ms until it gives `Some`. `None` when the engine stopped on
/// a fatal error first, or `limit` passed (without one it waits as long as that takes).
pub fn poll<B: Backend, T>(
    engine: &Engine<B>,
    limit: Option<Duration>,
    f: impl Fn(&Model) -> Option<T>,
) -> Option<T> {
    let t0 = Instant::now();
    loop {
        let (v, fatal) = engine.read(|m| (f(m), m.status.fatal.is_some()));
        if v.is_some() || fatal {
            return v;
        }
        if limit.is_some_and(|l| t0.elapsed() > l) {
            return None;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use dfdu_rules::{Env, Platform};
    use dfdu_scan::PortableBackend;

    use super::*;

    #[test]
    fn headless_engines_abandon_stalls_and_delete_only_with_a_journal() {
        let ro = headless(false);
        assert_eq!(ro.auto_abandon, Some(STALL_ABANDON));
        assert!(ro.read_only);
        assert!(ro.journal_dir.is_none());
        assert!(!headless(true).read_only);
    }

    #[test]
    fn threads_given_override_the_storage_default() {
        let dir = tempfile::tempdir().expect("temp dir");
        let (storage, auto) = for_storage(dir.path(), None, EngineOptions::default());
        assert_eq!(auto.workers, dfdu_engine::workers_for(storage));
        let (_, given) = for_storage(dir.path(), Some(3), EngineOptions::default());
        assert_eq!(given.workers, 3);
        let local = Local {
            mft: false,
            threads: Some(2),
            job: JobOptions::default(),
            rules: None,
        };
        let (_, own) = local.engine_options(dir.path(), EngineOptions::default());
        assert_eq!(own.workers, 2);
    }

    #[test]
    fn a_scan_classifies_only_with_rules_and_keeps_its_mount_options() {
        let dir = tempfile::tempdir().expect("temp dir");
        let local = Local {
            mft: false,
            threads: None,
            job: JobOptions {
                cross_mounts: true,
                ..JobOptions::default()
            },
            rules: None,
        };
        let plain = local.job(dir.path());
        assert!(plain.cross_mounts && plain.class.is_none());
        let rules = Arc::new(dfdu_rules::builtin(&Env::with(Platform::Linux, &[])).0);
        let local = Local {
            rules: Some(rules.clone()),
            ..local
        };
        let classified = local.job(dir.path());
        assert!(classified.cross_mounts);
        assert!(
            classified
                .class
                .is_some_and(|c| Arc::ptr_eq(&c.rules, &rules))
        );
    }

    #[test]
    fn run_scan_returns_once_the_tree_is_complete() {
        let dir = tempfile::tempdir().expect("temp dir");
        fs::create_dir(dir.path().join("sub")).expect("mkdir");
        fs::write(dir.path().join("sub/f"), b"x").expect("write");
        let engine = Engine::start(PortableBackend::default(), headless(false));
        let scan = Command::Scan {
            path: dir.path().to_path_buf().into(),
            opts: JobOptions::default(),
        };
        let engine = run_scan(engine, scan).expect("scanned");
        let items = engine.read(|m| m.tree.roots().next().map(|r| m.tree.total(r).items));
        engine.shutdown(SHUTDOWN);
        assert_eq!(items, Some(3));
    }

    #[test]
    fn poll_returns_what_it_reads_or_none_once_its_limit_passed() {
        let engine = Engine::start(PortableBackend::default(), headless(false));
        let limit = Some(Duration::from_millis(30));
        let t0 = Instant::now();
        assert_eq!(poll(&engine, limit, |_| None::<()>), None);
        assert!(t0.elapsed() >= Duration::from_millis(30));
        assert_eq!(
            poll(&engine, None, |m| Some(m.tree.roots().count())),
            Some(0)
        );
        engine.shutdown(SHUTDOWN);
    }
}
