//! The views beside the list: the map, whose tiles a click selects and a double click opens,
//! and the largest files below the directory, which ticks and shows them.

use dfdu_session::largest;
use dfdu_session::view::View;
use egui::Key;
use egui_kittest::kittest::Queryable;

use super::{
    close, files, harness, local, scan_done, selected, shown, shown_part, state, until, window,
};

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

#[test]
fn largest_lists_the_biggest_file_first_and_ticks_and_shows_it() {
    let tmp = files();
    let deep = tmp.path().join("a").join("b");
    std::fs::create_dir_all(&deep).expect("mkdir");
    std::fs::write(deep.join("huge.bin"), vec![1u8; 256 * 1024]).expect("write");
    let state_dir = tempfile::tempdir().expect("temp dir");
    let mut h = harness(local(tmp.path(), false, Some(state_dir.path().join("ops"))));
    until(&mut h, "the scan", |h| scan_done(h) && shown(h, "big.bin"));

    // `5` lists the files below, however deep, the largest first.
    h.event(egui::Event::Text("5".into()));
    until(&mut h, "the walk", |h| {
        state(h).largest.walked.is_some() && shown_part(h, "huge.bin")
    });
    let s = state(&h);
    assert_eq!(s.current_view(), View::Largest);
    let sizes: Vec<u64> = s.largest.files.iter().map(|f| f.size).collect();
    assert!(sizes.windows(2).all(|w| w[0] >= w[1]), "{sizes:?}");
    let huge = s.snap.dir_path.join(b"a").join(b"b").join(b"huge.bin");
    let (first, _) = largest::selected(s).expect("a file");
    assert_eq!(first, huge);
    assert!(shown_part(&h, "The largest 3 of 3 files below"));

    // Space ticks it and moves down; Up and Enter show it in Files, in its directory.
    h.key_press(Key::Space);
    until(&mut h, "the tick", |h| shown(h, "Unmark"));
    assert_eq!(state(&h).largest.cursor, 1);
    h.key_press(Key::ArrowUp);
    h.key_press(Key::Enter);
    until(&mut h, "Files", |h| {
        state(h).current_view() == View::Files && selected(h).as_deref() == Some(&b"huge.bin"[..])
    });
    assert_eq!(
        state(&h).snap.dir_path,
        huge.parent().expect("its directory")
    );
    close(h);
}
