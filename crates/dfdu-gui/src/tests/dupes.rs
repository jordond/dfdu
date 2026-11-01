//! The Duplicates view (gate 10): nothing is compared until asked; then the copy the keep rule
//! keeps is never ticked, by a group's tick, by the suggestion or when the rule changes; and
//! groups of clones, which free nothing, are left alone by the suggestion.

use std::path::Path;

use dfdu_session::dupes;
use dfdu_session::view::View;
use egui::Key;
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;

use super::{Window, age, close, harness, local, scan_done, shown, shown_part, state, until};

/// The size of the files compared: above the 1 MiB a comparison starts from.
const SIZE: usize = (1 << 20) + 4096;

/// Writes `SIZE` bytes of `fill` to `p`.
fn write(p: &Path, fill: u8) {
    std::fs::create_dir_all(p.parent().expect("a parent")).expect("mkdir");
    std::fs::write(p, vec![fill; SIZE]).expect("write");
}

/// Whether the file at `rel` below the scan root is ticked. The scan's root is the directory's
/// path as the scan resolved it, which the result's paths start with.
fn ticked(h: &Harness<Window>, rel: &Path) -> bool {
    let s = state(h);
    let node = (s.extras.dupes.iter())
        .find(|(p, _)| p.ends_with(rel))
        .and_then(|(_, n)| *n)
        .expect("in the tree");
    s.marks.contains(node)
}

#[test]
fn duplicates_never_tick_the_copy_kept() {
    let tmp = tempfile::tempdir().expect("temp dir");
    let root = tmp.path().to_path_buf();
    let (a1, a2, a3) = (
        Path::new("a1.bin"),
        &Path::new("x").join("a2.bin"),
        &Path::new("y").join("z").join("a3.bin"),
    );
    for p in [a1, a2, a3] {
        write(&root.join(p), 1);
    }
    age(&root.join(a1), 30);
    age(&root.join(a2), 10);
    let (b1, b2) = (Path::new("b1.bin"), Path::new("b2.bin"));
    write(&root.join(b1), 2);
    write(&root.join(b2), 2);
    let journal = tempfile::tempdir().expect("temp dir");
    let mut h = harness(local(&root, false, Some(journal.path().join("ops"))));
    until(&mut h, "the scan", scan_done);

    // `4` shows the view, which compares nothing until asked.
    h.event(egui::Event::Text("4".into()));
    until(&mut h, "the count", |h| shown(h, "Compare now"));
    assert_eq!(state(&h).current_view(), View::Duplicates);
    assert!(state(&h).dupes.run.is_none());
    assert!(shown_part(&h, "5 files share their size with another"));
    h.get_by_label("Compare now").click();
    until(&mut h, "the groups", |h| {
        dupes::rows(state(h)).len() == 7 && shown_part(h, "2 groups · ")
    });
    assert!(shown(&h, "kept"));

    // The suggestion ticks every copy but the oldest (or, of equals, the first by path).
    h.get_by_label("Tick suggested").click();
    until(&mut h, "the suggestion", |h| {
        shown_part(h, "marked 3 copies (the oldest file of each group is kept)")
    });
    assert!(!ticked(&h, a1) && ticked(&h, a2) && ticked(&h, a3));
    assert!(!ticked(&h, b1) && ticked(&h, b2));

    // Keeping the newest instead unticks the newest, which is now kept.
    h.event(egui::Event::Text("K".into()));
    until(&mut h, "the keep rule", |h| {
        shown_part(h, "keep the newest copy")
    });
    assert_eq!(state(&h).dupes.keep, dupes::Keep::Newest);
    assert!(!ticked(&h, a3) && ticked(&h, a2));
    h.event(egui::Event::Text("A".into()));
    until(&mut h, "the suggestion", |h| ticked(h, a1));
    assert!(!ticked(&h, a3));

    // A group's tick ticks every copy but the one kept, and again unticks them.
    h.event(egui::Event::Text("X".into()));
    h.key_press(Key::Home);
    h.key_press(Key::Space);
    until(&mut h, "the group's tick", |h| {
        ticked(h, a1) && ticked(h, a2)
    });
    assert!(!ticked(&h, a3), "the copy kept");

    // Leaving the view and coming back keeps what was found.
    h.event(egui::Event::Text("1".into()));
    h.event(egui::Event::Text("4".into()));
    until(&mut h, "the groups again", |h| {
        dupes::rows(state(h)).len() == 7
    });
    close(h);
}

/// Clones share their data: deleting one frees nothing, and the suggestion never ticks one.
/// Only where `cp -c` makes a clone (APFS).
#[cfg(target_os = "macos")]
#[test]
fn the_suggestion_leaves_clones_alone() {
    let tmp = tempfile::tempdir().expect("temp dir");
    let root = tmp.path().to_path_buf();
    let (c1, c2) = (Path::new("c1.bin"), Path::new("c2.bin"));
    write(&root.join(c1), 3);
    let cloned = std::process::Command::new("cp")
        .arg("-c")
        .args([root.join(c1), root.join(c2)])
        .status()
        .is_ok_and(|s| s.success());
    if !cloned {
        return;
    }
    let d2 = Path::new("d2.bin");
    write(&root.join("d1.bin"), 4);
    write(&root.join(d2), 4);
    let journal = tempfile::tempdir().expect("temp dir");
    let mut h = harness(local(&root, false, Some(journal.path().join("ops"))));
    until(&mut h, "the scan", scan_done);
    h.event(egui::Event::Text("4".into()));
    until(&mut h, "the count", |h| shown(h, "Compare now"));
    h.key_press(Key::Enter);
    until(&mut h, "the groups", |h| dupes::found(state(h)).is_some());
    h.event(egui::Event::Text("A".into()));
    until(&mut h, "the suggestion", |h| ticked(h, d2));
    // A group of clones is listed as freeing nothing, or not compared at all.
    let marked = |rel: &Path| {
        (state(&h).marks.items().iter()).any(|m| m.target.path.to_local().ends_with(rel))
    };
    assert!(!marked(c1) && !marked(c2));
    close(h);
}
