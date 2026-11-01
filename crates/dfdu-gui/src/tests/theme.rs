//! A theme file read at the start draws the window with its RGB tokens on its base's dark or
//! light theme; the View menu and the palette offer it beside the built-in themes, and every
//! other theme file of the theme directory, read when it is chosen.

use dfdu_session::palette::ThemeFile;
use egui::{Color32, Event};
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;

use super::{
    Window, close, files, harness_themed, harness_with, scan_done, settle, shown, shown_part, until,
};
use crate::settings::{Prefs, ThemeChoice};

/// The colour of text in the theme shown.
fn text_color(h: &Harness<Window>) -> Option<Color32> {
    h.ctx.global_style().visuals.override_text_color
}

#[test]
fn a_theme_file_colours_the_window_and_is_offered_beside_the_built_in_themes() {
    let tmp = files();
    let file = ThemeFile::parse("base = \"light\"\n[tokens]\nfg = \"#2e3440\"\n").expect("parses");
    let prefs = Prefs {
        theme: ThemeChoice::File("nord".into()),
        ..Prefs::default()
    };
    let mut h = harness_with(super::local(tmp.path(), true, None), prefs, Some(file));
    until(&mut h, "the scan", |h| scan_done(h) && shown(h, "big.bin"));
    let nord = Some(Color32::from_rgb(0x2e, 0x34, 0x40));
    assert_eq!(h.ctx.theme(), egui::Theme::Light, "the file's base");
    assert_eq!(text_color(&h), nord);

    // The View menu lists the built-in themes and the file; dark is the built-in one.
    h.get_by_label("View").click();
    settle(&mut h);
    assert!(shown(&h, "system") && shown(&h, "light") && shown(&h, "nord"));
    h.get_by_label("dark").click();
    until(&mut h, "the dark theme", |h| {
        h.ctx.theme() == egui::Theme::Dark
    });
    settle(&mut h);
    assert_ne!(text_color(&h), nord);

    // The palette chooses it again by its name.
    h.event(Event::Key {
        key: egui::Key::Escape,
        physical_key: None,
        pressed: true,
        repeat: false,
        modifiers: egui::Modifiers::NONE,
    });
    settle(&mut h);
    h.event(Event::Text(":".into()));
    until(&mut h, "the palette", |h| shown_part(h, " commands"));
    h.event(Event::Text("nord".into()));
    until(&mut h, "the entry", |h| shown(h, "Theme: nord"));
    h.key_press(egui::Key::Enter);
    until(&mut h, "the file's theme", |h| {
        h.ctx.theme() == egui::Theme::Light && text_color(h) == nord
    });
    let gui = h.state().gui.as_ref().expect("the window");
    assert_eq!(gui.prefs().theme, ThemeChoice::File("nord".into()));
    close(h);
}

#[test]
fn the_view_menu_lists_every_theme_file_and_reads_one_when_chosen() {
    let tmp = files();
    let themes = tempfile::tempdir().expect("temp dir");
    let write = |name: &str, text: &str| std::fs::write(themes.path().join(name), text);
    write(
        "solar.toml",
        "base = \"light\"\n[tokens]\nfg = \"#586e75\"\n",
    )
    .expect("write");
    write("broken.toml", "base = \"sepia\"\n").expect("write");
    let launch = super::local(tmp.path(), true, None);
    let dir = Some(themes.path().to_path_buf());
    let mut h = harness_themed(launch, Prefs::default(), None, dir);
    until(&mut h, "the scan", |h| scan_done(h) && shown(h, "big.bin"));
    h.get_by_label("View").click();
    settle(&mut h);
    assert!(shown(&h, "solar") && shown(&h, "broken"));
    h.get_by_label("solar").click();
    until(&mut h, "the file's theme", |h| {
        text_color(h) == Some(Color32::from_rgb(0x58, 0x6e, 0x75))
    });
    assert_eq!(h.ctx.theme(), egui::Theme::Light, "the file's base");
    // A file that does not read leaves the theme as it was, and says why.
    h.key_press(egui::Key::Escape);
    settle(&mut h);
    h.get_by_label("View").click();
    settle(&mut h);
    h.get_by_label("broken").click();
    until(&mut h, "the refusal", |h| shown_part(h, "theme broken: "));
    let gui = h.state().gui.as_ref().expect("the window");
    assert_eq!(gui.prefs().theme, ThemeChoice::File("solar".into()));
    close(h);
}
