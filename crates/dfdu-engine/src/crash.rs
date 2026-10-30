//! Crash reports: what the engine writes when the applier panics, and what a panic hook on
//! another thread (the UI) can add to its own report.

use std::path::PathBuf;
use std::sync::Weak;
use std::time::Duration;

use parking_lot::RwLock;

use crate::applier::Model;

/// The state directory's `crash` folder ([`dfdu_rules::dirs::crash_dir`]).
#[must_use]
pub fn default_crash_dir() -> Option<PathBuf> {
    dfdu_rules::dirs::crash_dir()
}

fn crash_report(msg: &str, m: &Model) -> String {
    format!(
        "dfdu {} crash report\n\nthread: dfdu-applier\npanic: {msg}\n\nbacktrace:\n{}\n\n{}",
        env!("CARGO_PKG_VERSION"),
        std::backtrace::Backtrace::force_capture(),
        crash_details(m),
    )
}

/// The engine's part of a crash report: counters, anomalies, jobs and the last messages and
/// commands applied.
fn crash_details(m: &Model) -> String {
    format!(
        "stats: {:?}\nanomalies: {:?}\njobs: {:?}\n\n{}",
        m.status.stats,
        m.status.anomalies,
        m.status.jobs,
        m.recent.report(),
    )
}

/// The model of the engine started last, for a panic hook on another thread.
pub(crate) static CRASH_MODEL: parking_lot::Mutex<Option<Weak<RwLock<Model>>>> =
    parking_lot::Mutex::new(None);

/// `crash_details` of the engine started last, for a panic on a thread other than the
/// applier (the UI); `None` if no engine is running. Waits at most 500 ms for the read lock: a
/// thread that panics while holding it would wait behind the applier's pending write forever.
pub fn crash_state() -> Option<String> {
    let model = CRASH_MODEL
        .try_lock_for(Duration::from_millis(100))?
        .as_ref()?
        .upgrade()?;
    Some(match model.try_read_for(Duration::from_millis(500)) {
        Some(g) => crash_details(&g),
        None => "engine state: not read, the model stayed locked for 500 ms\n".to_string(),
    })
}

/// Whether a panic on the thread with this name is caught and recorded by the engine: a scan
/// worker's (recorded as a `Panicked` directory) or the applier's (recorded as the engine's fatal
/// error, with its own crash report). A panic hook that writes a crash report or restores the
/// terminal must leave these alone.
#[must_use]
pub fn panic_is_caught_by_engine(thread_name: Option<&str>) -> bool {
    // scan-cache: a resume's journal query panic is caught too (it rescans everything).
    matches!(
        thread_name,
        Some("dfdu-worker" | "dfdu-applier" | "dfdu-journal")
    )
}

/// Writes a crash report into `dir`; returns its path.
#[must_use]
pub fn write_crash_report(dir: &std::path::Path, report: &str) -> Option<PathBuf> {
    std::fs::create_dir_all(dir).ok()?;
    let t = dfdu_tree::now_secs();
    let path = dir.join(format!("crash-{t}-{}.txt", std::process::id()));
    std::fs::write(&path, report).ok()?;
    Some(path)
}

/// Records an applier panic: the model's fatal error, with the crash report written in
/// `crash_dir`.
pub(crate) fn record_applier_panic(
    model: &RwLock<Model>,
    crash_dir: Option<&std::path::Path>,
    msg: &str,
) {
    let mut g = model.write();
    let report = crash_report(msg, &g);
    let file = crash_dir.and_then(|d| write_crash_report(d, &report));
    g.status.fatal = Some(match file {
        Some(f) => format!("internal error: {msg} (report: {})", f.display()),
        None => format!("internal error: {msg}"),
    });
}
