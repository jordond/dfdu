//! The start screen takes a folder dropped on the window, and a scan of this machine is recorded
//! in the scan history in dfdu's state directory, as `dfdu` records it. The history test runs
//! this test binary again with `DFDU_STATE_DIR` set, because changing the environment of a
//! running process needs `unsafe`.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::time::{Duration, Instant};

use dfdu_session::startup::Source;
use egui_kittest::kittest::Queryable;

use super::{PATIENCE, close, files, harness, local, scan_done, shown, until};
use crate::args::Args;
use crate::launch::{self, Launch};

/// A path dropped on the window.
#[derive(Debug)]
struct Dropped(PathBuf);

impl egui::DroppedFile for Dropped {
    fn path(&self) -> &Path {
        &self.0
    }

    fn bytes(&self) -> Result<Vec<u8>, String> {
        Err("not read".into())
    }
}

#[test]
fn a_folder_dropped_on_the_start_screen_is_scanned() {
    let tmp = files();
    let start = Launch {
        source: Source::Local {
            path: None,
            start: launch::local(None),
        },
        ..local(tmp.path(), true, None)
    };
    let mut h = harness(start);
    until(&mut h, "the start screen", |h| shown(h, "Open folder…"));

    // A file is not a folder: the screen says so.
    let file = tmp.path().join("big.bin");
    h.input_mut().dropped_files.push(Arc::new(Dropped(file)));
    until(&mut h, "the note", |h| {
        h.query_by_label_contains("is not a folder").is_some()
    });
    assert!(
        !h.state()
            .gui
            .as_ref()
            .is_some_and(crate::app::Gui::browsing)
    );

    let dir = tmp.path().to_path_buf();
    h.input_mut().dropped_files.push(Arc::new(Dropped(dir)));
    until(&mut h, "the scan", |h| scan_done(h) && shown(h, "big.bin"));
    close(h);
}

/// Set to the directory to scan when this binary runs the inner test.
const INNER: &str = "DFDU_GUI_HISTORY_TEST_DIR";

#[test]
fn a_scan_is_recorded_in_the_history_of_the_state_directory() {
    if std::env::var_os(INNER).is_some() {
        return;
    }
    let tmp = files();
    let state_dir = tempfile::tempdir().expect("temp dir");
    let out = Command::new(std::env::current_exe().expect("the test binary"))
        .args(["tests::start::inner_record", "--exact", "--nocapture"])
        .env(INNER, tmp.path())
        .env(dfdu_sys::dirs::STATE_DIR_VAR, state_dir.path())
        .env("DFDU_CACHE", "0")
        .output()
        .expect("runs");
    assert!(
        out.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        String::from_utf8_lossy(&out.stdout).contains("1 passed"),
        "the inner test ran"
    );
    let db = state_dir.path().join("history.sqlite3");
    let history = dfdu_store::history::History::open(&db).expect("the history");
    let scans = history.scans(None).expect("its scans");
    assert_eq!(scans.len(), 1, "{scans:?}");
}

/// Run by the test above, in a process with `DFDU_STATE_DIR` set: opens the directory as the
/// command line does, and waits for its scan to be recorded.
#[test]
fn inner_record() {
    let Some(dir) = std::env::var_os(INNER) else {
        return;
    };
    let args = Args {
        path: Some(dir.into()),
        read_only: false,
        import: None,
        no_rules: true,
        theme: None,
    };
    let launched = launch::prepare(&args).expect("prepares");
    assert!(
        launched.settings.record,
        "a scan of this machine is recorded"
    );
    let mut h = harness(launched);
    until(&mut h, "the scan", |h| scan_done(h) && shown(h, "big.bin"));
    let db = dfdu_sys::dirs::history_path().expect("a history path");
    let deadline = Instant::now() + PATIENCE;
    let recorded = || {
        dfdu_store::history::History::open(&db)
            .and_then(|h| h.scans(None))
            .is_ok_and(|s| !s.is_empty())
    };
    while !recorded() {
        assert!(Instant::now() < deadline, "waited too long for the record");
        h.step();
        std::thread::sleep(Duration::from_millis(20));
    }
    close(h);
}
