//! Starting engines on this host's files, for the headless commands and the browser alike, and
//! running a headless command's scan to the end.

use std::path::Path;
use std::process::ExitCode;
use std::sync::Arc;
use std::time::Duration;

use dfdu_engine::{Command, Engine, EngineOptions, WaitIdle};
use dfdu_rules::RuleSet;
use dfdu_scan::classify::ClassOpts;
use dfdu_scan::{Backend, JobOptions, NativeBackend, Storage, native_backend};

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
            dfdu_clean::default_journal_dir()
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
/// directory is on, with as many workers as suit it, classifying with the rules.
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
    /// A scan of `path` with the mount options in `mounts`, saying why --mft cannot be used.
    pub fn new(
        path: &Path,
        mounts: MountArgs,
        threads: Option<usize>,
        rules: Option<Arc<RuleSet>>,
    ) -> Local {
        Local {
            mft: mounts.use_mft(path),
            threads,
            job: mounts.job(),
            rules,
        }
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
        let (storage, opts) = for_storage(path, self.threads, opts);
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
    let (engine, job) = Local::new(path, mounts, threads, rules).start(path, headless(journal));
    run_scan(
        engine,
        Command::Scan {
            path: path.to_path_buf(),
            opts: job,
        },
    )
}

/// Sends `cmd` (a scan or a resume) and waits until the engine is done with it. An engine that
/// failed, or a link to the agent that ended before anything arrived, ends the command: the
/// reason is reported, the engine stopped and exit code 1 returned. A link that ended later is
/// reported and leaves an incomplete tree to print.
pub fn run_scan<B: Backend>(engine: Engine<B>, cmd: Command) -> Result<Engine<B>, ExitCode> {
    engine.send(cmd);
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
