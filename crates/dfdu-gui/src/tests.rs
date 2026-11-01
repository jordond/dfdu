//! The window driven headless through its accessibility tree (`egui_kittest`), on temporary
//! directories: browsing (here), marking and deleting with undo (`flows`), the gates of a
//! read-only browser and of an imported scan (`gates`), the start screen and the history
//! (`start`), the map and the largest files (`views`), the Cleanup view with its native command
//! pane (`cleanup`), the Duplicates view (`dupes`), the command palette, the help and the
//! platform's shortcuts (`palette`), and the themes (`theme`). Nothing is moved to the system's Trash: the delete flows stage
//! instead, and the Trash is tried only up to its dialog. The native commands the tests run are
//! harmless ones of rules written for them (`ls`, `sleep`, `true`).

mod cleanup;
mod dupes;
mod flows;
mod gates;
mod palette;
mod start;
mod theme;
mod views;

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

use dfdu_engine::EngineOptions;
use dfdu_session::SessionState;
use dfdu_session::palette::ThemeFile;
use dfdu_session::startup::Source;
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;

use crate::app::Gui;
use crate::launch::{self, Launch, Settings};
use crate::settings::Prefs;

/// The longest a test waits for the window to show something.
const PATIENCE: Duration = Duration::from_secs(30);

/// The window's state in the harness: the launch until the first frame creates the window with
/// the harness's context.
struct Window {
    launch: Option<(Launch, Prefs, Option<ThemeFile>)>,
    gui: Option<Gui>,
}

/// A browser of `dir`, scanned without rules and recording nothing; it deletes, journalled in
/// `journal`, unless `read_only`.
fn local(dir: &Path, read_only: bool, journal: Option<PathBuf>) -> Launch {
    Launch {
        source: Source::Local {
            path: Some(dir.to_path_buf()),
            start: launch::local(None),
        },
        settings: Settings {
            engine: EngineOptions {
                read_only,
                journal_dir: journal,
                ..EngineOptions::default()
            },
            record: false,
        },
    }
}

/// A browser of `dir` as [`local`] makes it, classifying with the rules of `rules`, a rule
/// file's text, alone.
fn with_rules(dir: &Path, read_only: bool, journal: Option<PathBuf>, rules: &str) -> Launch {
    let files = [(dfdu_rules::Source::Builtin, rules.to_owned())];
    let (set, errors) = dfdu_rules::load_from(&dfdu_rules::Env::current(), &files);
    assert!(errors.is_empty(), "{errors:?}");
    let mut l = local(dir, read_only, journal);
    l.source = Source::Local {
        path: Some(dir.to_path_buf()),
        start: launch::local(Some(Arc::new(set))),
    };
    l
}

/// Sets the modification time of `p` (a file or directory) `days` back.
fn age(p: &Path, days: u64) {
    let t = SystemTime::now() - Duration::from_secs(days * 86_400);
    let mut o = std::fs::OpenOptions::new();
    o.read(true);
    // Windows opens a directory only with FILE_FLAG_BACKUP_SEMANTICS, and setting its times
    // needs FILE_WRITE_ATTRIBUTES.
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        o.access_mode(0x0100).custom_flags(0x0200_0000);
    }
    o.open(p)
        .and_then(|f| f.set_modified(t))
        .expect("set the time");
}

/// The window of `launch`.
fn harness(launch: Launch) -> Harness<'static, Window> {
    harness_with(launch, Prefs::default(), None)
}

/// The window of `launch` with the settings `prefs` and the theme file `file` read for them.
fn harness_with(launch: Launch, prefs: Prefs, file: Option<ThemeFile>) -> Harness<'static, Window> {
    let state = Window {
        launch: Some((launch, prefs, file)),
        gui: None,
    };
    Harness::builder()
        .with_size(egui::vec2(1000.0, 640.0))
        // A frame's time as a window's, so two clicks in a row are a double click.
        .with_step_dt(1.0 / 60.0)
        .build_ui_state(
            |ui, w: &mut Window| {
                let gui = match &mut w.gui {
                    Some(gui) => gui,
                    None => {
                        let (launch, prefs, file) = w.launch.take().expect("a launch");
                        w.gui.insert(Gui::new(launch, prefs, file, ui.ctx()))
                    }
                };
                gui.show(ui);
            },
            state,
        )
}

/// A window browsing `dir` read-only.
fn window(dir: &Path) -> Harness<'static, Window> {
    harness(local(dir, true, None))
}

/// Runs frames until `done` holds for the harness, as the window does: the bridge thread and
/// the session's wake-ups ask for them.
fn until(h: &mut Harness<'static, Window>, what: &str, done: impl Fn(&Harness<Window>) -> bool) {
    let deadline = Instant::now() + PATIENCE;
    loop {
        h.step();
        if done(h) {
            return;
        }
        assert!(Instant::now() < deadline, "waited too long for {what}");
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// Something on screen is named `label`: a row, a button, or the selected entry in the status
/// bar.
fn shown(h: &Harness<Window>, label: &str) -> bool {
    h.query_all_by_label(label).next().is_some()
}

/// Something on screen has `part` in its name.
fn shown_part(h: &Harness<Window>, part: &str) -> bool {
    h.query_all_by_label_contains(part).next().is_some()
}

fn scan_done(h: &Harness<Window>) -> bool {
    h.state().gui.as_ref().is_some_and(Gui::scan_done)
}

/// The browser's state.
fn state<'h>(h: &'h Harness<Window>) -> &'h SessionState {
    let gui = h.state().gui.as_ref().expect("the window");
    gui.browser().expect("a browser").state()
}

/// The name of the selected entry.
fn selected(h: &Harness<Window>) -> Option<Vec<u8>> {
    state(h).selected_entry().map(|e| e.name.to_vec())
}

/// Stops the window's browser.
fn close(h: Harness<'static, Window>) {
    let mut gui = h.into_state().gui.expect("the window");
    gui.close();
}

/// Runs a few frames, for a window whose size changed to settle where it is drawn.
fn settle(h: &mut Harness<'static, Window>) {
    for _ in 0..3 {
        h.step();
    }
}

/// A directory with a large file and a directory with a file in it.
fn files() -> tempfile::TempDir {
    let tmp = tempfile::tempdir().expect("temp dir");
    std::fs::write(tmp.path().join("big.bin"), vec![1u8; 64 * 1024]).expect("write");
    std::fs::create_dir(tmp.path().join("sub")).expect("mkdir");
    std::fs::write(tmp.path().join("sub").join("inner.txt"), b"hello").expect("write");
    tmp
}

#[test]
fn the_window_scans_a_directory_and_opens_one_with_the_keys() {
    let tmp = files();
    let mut h = window(tmp.path());
    until(&mut h, "the scan", |h| {
        scan_done(h) && shown(h, "big.bin") && shown(h, "sub/")
    });
    assert!(!shown(&h, "inner.txt"));

    // The largest entry is first, and selected; `j` moves down to the directory, `Enter`
    // opens it.
    assert_eq!(selected(&h).as_deref(), Some(&b"big.bin"[..]));
    h.event(egui::Event::Text("j".into()));
    h.step();
    assert_eq!(selected(&h).as_deref(), Some(&b"sub"[..]));
    h.key_press(egui::Key::Enter);
    until(&mut h, "the directory", |h| shown(h, "inner.txt"));

    // Backspace goes back up, with the directory come from selected.
    h.key_press(egui::Key::Backspace);
    until(&mut h, "the parent", |h| shown(h, "big.bin"));
    assert_eq!(selected(&h).as_deref(), Some(&b"sub"[..]));

    // Tree mode: `T`, then the triangle of the directory expands it in place.
    h.event(egui::Event::Text("T".into()));
    until(&mut h, "tree mode", |h| shown(h, "Expand"));
    assert!(!shown(&h, "inner.txt"));
    h.get_by_label("Expand").click();
    until(&mut h, "the expanded directory", |h| {
        shown(h, "inner.txt") && shown(h, "Collapse")
    });
    close(h);
}

#[test]
fn the_details_panel_shows_the_selected_entry() {
    let tmp = files();
    let mut h = window(tmp.path());
    until(&mut h, "the scan", |h| scan_done(h) && shown(h, "big.bin"));
    h.event(egui::Event::Text("i".into()));
    until(&mut h, "the details", |h| {
        shown(h, "big.bin") && shown_part(h, "on disk") && shown_part(h, "apparent")
    });
    // On a directory: its largest children.
    h.event(egui::Event::Text("j".into()));
    until(&mut h, "the directory's details", |h| {
        shown(h, "sub/") && shown_part(h, "Largest by")
    });
    // The toolbar's box closes it again.
    h.get_by_label("Details").click();
    until(&mut h, "no details", |h| !shown_part(h, "Largest by"));
    close(h);
}
