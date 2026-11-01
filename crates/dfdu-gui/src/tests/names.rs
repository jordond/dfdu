//! Every control has an accessible name: a walk of the AccessKit tree of each view and window
//! finds no button, tick, text field, link or clickable row without one, and the rows of the
//! tables are in the tree, named for what they show.

use std::path::Path;

use egui::accesskit::{Action, Role};
use egui_kittest::Harness;
use egui_kittest::kittest::{NodeT, Queryable};

use super::{Window, close, harness, scan_done, settle, shown, shown_part, state, until};

/// Matched by the rules below: a cache directory.
const RULES: &str = r#"schema_version = 1
[[rule]]
id = "test.cache"
category = "sys.cache"
name = "Test cache"
safety = "safe"
action = "delete"
explanation = "A cache for the tests."
lost = "Nothing the tests need."
[rule.match]
kind = "dir"
names = ["cache"]
stop_descent = true
"#;

/// The controls on screen that a user can act on and that have no name: inputs (as egui
/// counts them) and anything else that takes a click, but scroll bars.
fn unnamed(h: &Harness<Window>) -> Vec<String> {
    h.root()
        .children_recursive()
        .map(|n| n.accesskit_node())
        .filter(|n| !n.is_hidden())
        .filter(|n| {
            let acts =
                egui::accessibility::is_input(n.role()) || n.data().supports_action(Action::Click);
            // egui's scroll bars are known by their role, as a platform's are.
            acts && !n.data().is_read_only() && n.role() != Role::ScrollBar
        })
        .filter(|n| {
            let label = n.label().is_some_and(|l| !l.trim().is_empty());
            let hint = (n.data().placeholder()).is_some_and(|p| !p.trim().is_empty());
            let value = n.role() == Role::Label && n.value().is_some_and(|v| !v.is_empty());
            !(label || hint || value)
        })
        .map(|n| {
            // The nearest ancestor with a name says where it is.
            let inside = std::iter::successors(n.parent(), |p| p.parent())
                .find_map(|p| p.label())
                .unwrap_or_default();
            format!("{:?} at {:?} inside {inside:?}", n.role(), n.raw_bounds())
        })
        .collect()
}

/// Asserts that everything on screen in `what` that acts has a name.
fn all_named(h: &mut Harness<'static, Window>, what: &str) {
    settle(h);
    let missing = unnamed(h);
    assert!(missing.is_empty(), "{what}: {}", missing.join("\n"));
}

/// The rows of the tables on screen, by their names.
fn rows(h: &Harness<Window>) -> Vec<String> {
    h.query_all_by_role(Role::Row)
        .filter_map(|n| n.accesskit_node().label())
        .collect()
}

fn write(p: &Path, bytes: usize, fill: u8) {
    std::fs::create_dir_all(p.parent().expect("a parent")).expect("mkdir");
    std::fs::write(p, vec![fill; bytes]).expect("write");
}

/// Types `text` and runs a frame.
fn typed(h: &mut Harness<'static, Window>, text: &str) {
    h.event(egui::Event::Text(text.into()));
    h.step();
}

#[test]
fn every_view_and_window_names_its_controls_and_rows() {
    let tmp = tempfile::tempdir().expect("temp dir");
    let root = tmp.path();
    // Two copies above the 1 MiB a comparison of duplicates starts from, and a cache.
    let copy = (1 << 20) + 4096;
    write(&root.join("a.bin"), copy, 7);
    write(&root.join("sub").join("b.bin"), copy, 7);
    write(&root.join("cache").join("blob"), 64 * 1024, 1);
    let journal = tempfile::tempdir().expect("temp dir");
    let launch = super::with_rules(root, false, Some(journal.path().join("ops")), RULES);
    let mut h = harness(launch);
    until(&mut h, "the scan", |h| scan_done(h) && shown(h, "a.bin"));

    // Files, in tree mode too; the rows are named for the entry and its size.
    all_named(&mut h, "Files");
    assert!(
        rows(&h).iter().any(|r| r.starts_with("a.bin, ")),
        "{:?}",
        rows(&h)
    );
    typed(&mut h, "T");
    all_named(&mut h, "tree mode");
    typed(&mut h, "T");
    h.get_by_label("View").click();
    all_named(&mut h, "the View menu");
    h.get_by_label("View").click();
    settle(&mut h);
    h.query_all_by_role(Role::Row)
        .find(|n| {
            n.accesskit_node()
                .label()
                .is_some_and(|l| l.starts_with("sub/"))
        })
        .expect("the directory's row")
        .click_secondary();
    until(&mut h, "the row's menu", |h| shown(h, "Copy path"));
    all_named(&mut h, "a row's menu");
    h.key_press(egui::Key::Escape);
    settle(&mut h);

    // The map, the largest files.
    typed(&mut h, "2");
    all_named(&mut h, "Map");
    typed(&mut h, "5");
    until(&mut h, "the largest files", |h| {
        !state(h).largest.files.is_empty()
    });
    all_named(&mut h, "Largest");
    assert!(
        rows(&h).iter().any(|r| r.contains("b.bin")),
        "{:?}",
        rows(&h)
    );

    // Cleanup, its category open.
    typed(&mut h, "3");
    until(&mut h, "the matches", |h| shown(h, "Safe to delete"));
    h.key_press(egui::Key::ArrowDown);
    h.key_press(egui::Key::Enter);
    until(&mut h, "the item", |h| {
        dfdu_session::cleanup::selected(state(h)).is_some() || shown(h, "Test cache")
    });
    all_named(&mut h, "Cleanup");
    assert!(
        rows(&h).iter().any(|r| r.starts_with("Safe to delete, ")),
        "{:?}",
        rows(&h)
    );

    // Duplicates, before and after a comparison.
    typed(&mut h, "4");
    until(&mut h, "the count", |h| shown(h, "Compare now"));
    all_named(&mut h, "Duplicates");
    h.get_by_label("Compare now").click();
    until(&mut h, "the groups", |h| shown_part(h, "1 group · "));
    all_named(&mut h, "the groups of duplicates");
    assert!(
        rows(&h).iter().any(|r| r.starts_with("2 copies of ")),
        "{:?}",
        rows(&h)
    );

    // The windows: the type picker, the help, the palette, the delete dialog.
    typed(&mut h, "1");
    typed(&mut h, "t");
    until(&mut h, "the type picker", |h| shown(h, "All files"));
    all_named(&mut h, "the type picker");
    h.key_press(egui::Key::Escape);
    typed(&mut h, "?");
    until(&mut h, "the help", |h| shown(h, "Navigate"));
    all_named(&mut h, "the help");
    h.key_press(egui::Key::Escape);
    typed(&mut h, ":");
    until(&mut h, "the palette", |h| shown_part(h, " commands"));
    all_named(&mut h, "the palette");
    h.key_press(egui::Key::Escape);
    settle(&mut h);
    typed(&mut h, "d");
    until(&mut h, "the delete dialog", |h| state(h).confirm.is_some());
    all_named(&mut h, "the delete dialog");
    h.key_press(egui::Key::Escape);
    settle(&mut h);

    // The banner on Full Disk Access.
    let gui = h.state_mut().gui.as_mut().expect("the window");
    gui.browser_mut().expect("a browser").view_mut().no_access =
        Some(vec!["~/Library/Mail".into()]);
    until(&mut h, "the banner", |h| shown(h, "Dismiss"));
    all_named(&mut h, "the banner");
    close(h);
}

#[test]
fn the_start_screen_names_its_controls() {
    let tmp = super::files();
    let start = crate::launch::Launch {
        source: dfdu_session::startup::Source::Local {
            path: None,
            start: crate::launch::local(None),
        },
        ..super::local(tmp.path(), true, None)
    };
    let mut h = harness(start);
    until(&mut h, "the start screen", |h| shown(h, "Open folder…"));
    all_named(&mut h, "the start screen");
}
