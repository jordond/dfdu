//! The views beside the list: the map, whose tiles a click selects and a double click opens.

use egui::Key;
use egui_kittest::kittest::Queryable;

use super::{close, files, scan_done, selected, shown, state, until, window};

#[test]
fn the_map_selects_a_tile_on_a_click_and_opens_it_on_a_double_click() {
    let tmp = files();
    let mut h = window(tmp.path());
    until(&mut h, "the scan", |h| scan_done(h) && shown(h, "big.bin"));

    // `2` shows the map, with a tile per entry, the largest selected.
    h.event(egui::Event::Text("2".into()));
    until(&mut h, "the map", |h| {
        state(h).map_drawn() && shown(h, "sub/") && shown(h, "Depth 1")
    });
    assert_eq!(selected(&h).as_deref(), Some(&b"big.bin"[..]));

    // A click selects a tile; an arrow moves back to the other one.
    h.get_by_label("sub/").click();
    until(&mut h, "the click", |h| {
        selected(h).as_deref() == Some(&b"sub"[..])
    });
    h.key_press(Key::ArrowLeft);
    h.key_press(Key::ArrowRight);
    h.step();
    assert!(selected(&h).is_some());

    // A double click opens the directory, which the map then shows.
    let sub = h.get_by_label("sub/");
    sub.click();
    sub.click();
    until(&mut h, "the directory", |h| {
        shown(h, "inner.txt") && state(h).snap.dir_path.to_local().ends_with("sub")
    });
    assert!(state(&h).map_drawn(), "still the map");

    // Backspace goes back up, and `1` back to the list.
    h.key_press(Key::Backspace);
    until(&mut h, "the parent", |h| shown(h, "big.bin"));
    h.event(egui::Event::Text("1".into()));
    until(&mut h, "the list", |h| {
        !state(h).map_drawn() && shown(h, "Size")
    });
    close(h);
}
