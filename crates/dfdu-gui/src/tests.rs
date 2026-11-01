//! The window driven headless through its accessibility tree (`egui_kittest`): it scans a small
//! directory, shows its rows by name, and opens a directory with the keys.

use std::path::Path;
use std::time::{Duration, Instant};

use dfdu_engine::EngineOptions;
use dfdu_session::startup::Source;
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;

use crate::app::Gui;
use crate::launch::{self, Launch, Settings};

/// The longest a test waits for the window to show something.
const PATIENCE: Duration = Duration::from_secs(30);

/// The window's state in the harness: the launch until the first frame creates the window with
/// the harness's context.
struct Window {
    launch: Option<Launch>,
    gui: Option<Gui>,
}

/// A window browsing `dir`, scanned without rules, recording nothing, and read-only.
fn window(dir: &Path) -> Harness<'static, Window> {
    let launch = Launch {
        source: Source::Local {
            path: Some(dir.to_path_buf()),
            start: launch::local(None),
        },
        settings: Settings {
            engine: EngineOptions {
                read_only: true,
                journal_dir: None,
                ..EngineOptions::default()
            },
            record: false,
        },
    };
    let state = Window {
        launch: Some(launch),
        gui: None,
    };
    Harness::builder()
        .with_size(egui::vec2(900.0, 600.0))
        .build_ui_state(
            |ui, w: &mut Window| {
                let gui = match &mut w.gui {
                    Some(gui) => gui,
                    None => {
                        let launch = w.launch.take().expect("a launch");
                        w.gui.insert(Gui::new(launch, ui.ctx()))
                    }
                };
                gui.show(ui);
            },
            state,
        )
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

/// Something on screen is named `label`: a row, or the selected entry in the status bar.
fn shown(h: &Harness<Window>, label: &str) -> bool {
    h.query_all_by_label(label).next().is_some()
}

fn scan_done(h: &Harness<Window>) -> bool {
    h.state().gui.as_ref().is_some_and(Gui::scan_done)
}

#[test]
fn the_window_scans_a_directory_and_opens_one_with_the_keys() {
    let tmp = tempfile::tempdir().expect("temp dir");
    std::fs::write(tmp.path().join("big.bin"), vec![1u8; 64 * 1024]).expect("write");
    std::fs::create_dir(tmp.path().join("sub")).expect("mkdir");
    std::fs::write(tmp.path().join("sub").join("inner.txt"), b"hello").expect("write");

    let mut h = window(tmp.path());
    until(&mut h, "the scan", |h| {
        scan_done(h) && shown(h, "big.bin") && shown(h, "sub/")
    });
    assert!(!shown(&h, "inner.txt"));

    // The largest entry is first, and selected; `j` moves down to the directory, `Enter`
    // opens it.
    let selected = |h: &Harness<Window>| {
        let gui = h.state().gui.as_ref().expect("the window");
        let s = gui.browser().expect("a browser").state();
        s.selected_entry().map(|e| e.name.to_vec())
    };
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

    let mut gui = h.into_state().gui.expect("the window");
    gui.close();
}
