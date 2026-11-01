//! The gates of a browser that deletes nothing: a read-only one shows no way to delete, mark or
//! put back (no button, no key, no menu item), and an imported scan, read-only too, refuses the
//! commands that would run on this machine (the file manager, a rescan, native cleanup
//! commands).

use egui::Key;
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;

use super::{Window, close, files, harness, scan_done, shown, shown_part, state, until, window};
use crate::args::Args;
use crate::launch;

/// The labels of every way to delete, mark or put back.
const DELETING: [&str; 8] = [
    "Move to Trash…",
    "Delete permanently…",
    "Undo",
    "Clear marks",
    "Nothing marked",
    "Mark",
    "Unmark",
    "Delete",
];

/// Nothing on screen deletes, marks or puts back.
fn no_way_to_delete(h: &Harness<Window>) {
    for label in DELETING {
        assert!(!shown(h, label), "{label:?} is shown");
    }
}

/// The keys that delete, mark or put back, pressed: nothing opens, nothing is marked.
fn keys_do_nothing(h: &mut Harness<'static, Window>) {
    for k in ["d", "D", "U"] {
        h.event(egui::Event::Text(k.into()));
        h.step();
    }
    h.key_press(Key::Delete);
    h.key_press(Key::Space);
    h.step();
    let s = state(h);
    assert!(s.confirm.is_none() && s.marks.is_empty());
    no_way_to_delete(h);
}

/// Opens the menu of the row named `name`.
fn menu(h: &mut Harness<'static, Window>, name: &str) {
    h.get_by_label(name).click_secondary();
    h.step();
    h.step();
    assert!(shown(h, "Copy path"), "the row's menu");
}

#[test]
fn a_read_only_browser_shows_no_way_to_delete() {
    let tmp = files();
    let mut h = window(tmp.path());
    until(&mut h, "the scan", |h| scan_done(h) && shown(h, "big.bin"));
    assert!(shown(&h, "read-only"));
    no_way_to_delete(&h);
    keys_do_nothing(&mut h);
    assert_eq!(
        state(&h).status.as_deref(),
        Some(dfdu_session::state::READ_ONLY),
        "Space says why nothing is marked"
    );
    menu(&mut h, "big.bin");
    assert!(shown(&h, "Show in file manager") && shown(&h, "Rescan"));
    no_way_to_delete(&h);
    assert!(tmp.path().join("big.bin").exists());
    close(h);
}

/// An ncdu export of `/nowhere` with a directory `d` and a file `f` in it.
pub const EXPORT: &[u8] = br#"[1,2,{},[{"name":"/nowhere"},[{"name":"d","dsize":4096},{"name":"f","asize":5,"dsize":4096}]]]"#;

#[test]
fn an_imported_scan_refuses_the_commands_of_this_machine() {
    let tmp = tempfile::tempdir().expect("temp dir");
    let file = tmp.path().join("scan.json");
    std::fs::write(&file, EXPORT).expect("write");
    let args = Args {
        path: None,
        read_only: false,
        import: Some(file),
        no_rules: false,
        theme: None,
    };
    let launched = launch::prepare(&args).expect("the export reads");
    assert!(!launched.settings.record, "an import is recorded nowhere");
    let mut h = harness(launched);
    until(&mut h, "the imported scan", |h| shown(h, "d/"));
    assert!(shown(&h, "read-only"), "an import is read-only");
    no_way_to_delete(&h);
    keys_do_nothing(&mut h);

    // No file manager here for another machine's paths: the menu leaves it out, and `o` says
    // why.
    menu(&mut h, "d/");
    assert!(!shown(&h, "Show in file manager"));
    h.key_press(Key::Escape);
    h.event(egui::Event::Text("o".into()));
    until(&mut h, "the refusal", |h| {
        shown_part(h, "this scan is remote or from a file")
    });

    // No rules classified it: no Cleanup view, and so no native command runs here.
    h.event(egui::Event::Text("3".into()));
    until(&mut h, "the refusal", |h| {
        shown(h, "no rules: the cleanup view needs classification")
    });
    h.event(egui::Event::Text("N".into()));
    until(&mut h, "the refusal", |h| {
        shown(h, "N works in Cleanup (3)")
    });
    assert!(state(&h).cleanup.pane.is_none());

    // The engine refuses to rescan it.
    h.event(egui::Event::Text("r".into()));
    until(&mut h, "the engine's refusal", |h| {
        (state(h).snap.notice.as_deref()).is_some_and(|n| n.contains("imported scan is read-only"))
    });
    close(h);
}
