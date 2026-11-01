//! The command palette runs an action or chooses a theme by name; the help lists the keys by
//! group and searches them; neither offers a way to delete in a read-only browser. The
//! platform's shortcuts open the palette and the filter and copy the selected path.

use dfdu_session::filter;
use egui::{Event, Key, Modifiers};
use egui_kittest::kittest::Queryable;

use super::{close, files, harness, local, scan_done, shown, shown_part, state, until, window};

#[test]
fn the_palette_runs_an_action_or_chooses_a_theme_by_name() {
    let tmp = files();
    let journal = tempfile::tempdir().expect("temp dir");
    let mut h = harness(local(tmp.path(), false, Some(journal.path().join("ops"))));
    until(&mut h, "the scan", |h| scan_done(h) && shown(h, "big.bin"));

    // Cmd-K (Ctrl-K) opens it with every action the window offers and the themes.
    h.key_press_modifiers(Modifiers::COMMAND, Key::K);
    until(&mut h, "the palette", |h| shown_part(h, " commands"));
    assert!(shown(&h, "move to Trash") && shown(&h, "Theme: light"));
    h.event(Event::Text("details".into()));
    until(&mut h, "the search", |h| {
        shown(h, "details of the selected entry") && !shown(h, "move to Trash")
    });
    h.key_press(Key::Enter);
    until(&mut h, "the details", |h| {
        state(h).details.open && !shown_part(h, " commands")
    });

    // `:` opens it too; a theme is chosen by its name.
    h.event(Event::Text(":".into()));
    until(&mut h, "the palette", |h| shown_part(h, " commands"));
    h.event(Event::Text("theme light".into()));
    until(&mut h, "the theme", |h| shown(h, "Theme: light"));
    h.key_press(Key::Enter);
    until(&mut h, "the light theme", |h| {
        h.ctx.theme() == egui::Theme::Light
    });

    // Esc closes it, running nothing.
    h.event(Event::Text(":".into()));
    until(&mut h, "the palette", |h| shown_part(h, " commands"));
    h.key_press(Key::Escape);
    until(&mut h, "no palette", |h| !shown_part(h, " commands"));
    close(h);
}

#[test]
fn the_help_lists_the_keys_by_group_and_searches_them() {
    let tmp = files();
    let journal = tempfile::tempdir().expect("temp dir");
    let mut h = harness(local(tmp.path(), false, Some(journal.path().join("ops"))));
    until(&mut h, "the scan", |h| scan_done(h) && shown(h, "big.bin"));
    h.event(Event::Text("?".into()));
    until(&mut h, "the help", |h| {
        shown(h, "Navigate") && shown(h, "Shortcuts") && shown(h, "move to Trash")
    });
    assert!(shown(&h, "Up k"), "the arrows are named");
    h.event(Event::Text("trash".into()));
    until(&mut h, "the search", |h| {
        !shown(h, "Navigate") && shown(h, "Act")
    });
    assert!(shown(&h, "move to Trash"));
    h.key_press(Key::Escape);
    until(&mut h, "no help", |h| !shown(h, "Act"));
    close(h);
}

#[test]
fn a_read_only_browser_offers_no_delete_in_the_help_or_the_palette() {
    let tmp = files();
    let mut h = window(tmp.path());
    until(&mut h, "the scan", |h| scan_done(h) && shown(h, "big.bin"));
    h.event(Event::Text("?".into()));
    until(&mut h, "the help", |h| shown(h, "Navigate"));
    assert!(!shown(&h, "move to Trash") && !shown(&h, "delete permanently (type delete)"));
    h.key_press(Key::Escape);
    until(&mut h, "no help", |h| !shown(h, "Navigate"));
    h.key_press_modifiers(Modifiers::COMMAND, Key::K);
    until(&mut h, "the palette", |h| shown_part(h, " commands"));
    h.event(Event::Text("trash".into()));
    h.step();
    h.step();
    assert!(!shown(&h, "move to Trash"));
    assert!(!shown_part(&h, "put back what was last moved"));
    close(h);
}

#[test]
fn the_platform_shortcuts_open_the_filter_and_copy_the_path() {
    let tmp = files();
    let mut h = window(tmp.path());
    until(&mut h, "the scan", |h| scan_done(h) && shown(h, "big.bin"));
    h.event(Event::Copy);
    until(&mut h, "the copy", |h| shown_part(h, "copied "));
    assert!(shown_part(&h, "big.bin"));
    h.key_press_modifiers(Modifiers::COMMAND, Key::F);
    until(&mut h, "the filter", |h| filter::editing(state(h)));
    h.event(Event::Text("sub".into()));
    until(&mut h, "the filtered list", |h| {
        shown(h, "sub/") && !h.query_all_by_label("big.bin").any(|_| true)
    });
    close(h);
}
