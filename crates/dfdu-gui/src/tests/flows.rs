//! Marking and deleting: scan, browse, mark, delete and undo; and the word a permanent delete
//! needs typed. The deletes stage (renamed aside in the temporary state directory, put back by
//! the undo) or delete for good inside a temporary directory; the Trash's dialog is opened and
//! cancelled, so the system's Trash is never touched.

use egui::Key;
use egui_kittest::kittest::{NodeT, Queryable};

use super::{
    close, files, harness, local, scan_done, selected, settle, shown, shown_part, state, until,
};

/// Whether the button named `label` is disabled.
fn disabled(h: &egui_kittest::Harness<super::Window>, label: &str) -> bool {
    h.get_by_label(label).accesskit_node().is_disabled()
}

#[test]
fn scan_browse_mark_stage_and_undo() {
    let tmp = files();
    let state_dir = tempfile::tempdir().expect("temp dir");
    let big = tmp.path().join("big.bin");
    let mut h = harness(local(tmp.path(), false, Some(state_dir.path().join("ops"))));
    until(&mut h, "the scan", |h| scan_done(h) && shown(h, "big.bin"));
    assert_eq!(selected(&h).as_deref(), Some(&b"big.bin"[..]));
    assert!(shown(&h, "Nothing marked"));

    // Space marks the selected entry and moves down; the bar below sums up the marks.
    h.key_press(Key::Space);
    until(&mut h, "the mark", |h| shown(h, "1 marked · 64.0 KiB"));
    assert_eq!(selected(&h).as_deref(), Some(&b"sub"[..]));
    assert!(shown(&h, "Unmark"), "the row's mark");

    // `d` asks to move the marks to the Trash, with what the engine estimates it frees, and
    // Cancel holding the keyboard: Enter cancels.
    h.event(egui::Event::Text("d".into()));
    until(&mut h, "the dialog", |h| {
        shown(h, "Move big.bin to the Trash?")
            && shown_part(h, "Frees about")
            && h.get_by_label("Cancel").is_focused()
    });
    h.key_press(Key::Enter);
    until(&mut h, "the dialog to close", |h| {
        state(h).confirm.is_none()
    });
    assert!(big.exists());

    // The Delete key opens it too; staging renames the file aside at once.
    h.key_press(Key::Delete);
    until(&mut h, "the dialog", |h| {
        shown(h, "Move big.bin to the Trash?")
    });
    settle(&mut h);
    h.get_by_label("Stage, then delete").click();
    until(&mut h, "staging", |h| {
        shown(h, "Stage big.bin for deletion?")
    });
    settle(&mut h);
    h.get_by_label("Stage").click();
    until(&mut h, "the staged file", |h| {
        !big.exists() && shown_part(h, "Staged 1 item (64.0 KiB): deleted in")
    });
    assert!(shown(&h, "Nothing marked"), "the mark went with the file");

    // Undo puts it back.
    settle(&mut h);
    h.get_by_label("Undo").click();
    until(&mut h, "the file put back", |h| {
        big.exists() && state(h).snap.undo.is_none() && shown(h, "big.bin")
    });
    close(h);
}

#[test]
fn a_permanent_delete_needs_the_word_typed() {
    let tmp = files();
    let state_dir = tempfile::tempdir().expect("temp dir");
    let big = tmp.path().join("big.bin");
    let mut h = harness(local(tmp.path(), false, Some(state_dir.path().join("ops"))));
    until(&mut h, "the scan", |h| scan_done(h) && shown(h, "big.bin"));
    let word = "Type delete to confirm:";

    // The button that deletes is disabled until the word is typed.
    h.event(egui::Event::Text("D".into()));
    until(&mut h, "the dialog", |h| {
        shown(h, "Permanently delete big.bin?") && shown(h, word)
    });
    assert!(disabled(&h, "Delete"));
    settle(&mut h);
    h.get_by_label(word).click();
    h.step();
    h.get_by_label(word).type_text("delet");
    h.step();
    h.step();
    assert!(disabled(&h, "Delete"), "a word that is not the word");

    // Enter with the wrong word deletes nothing, and says why.
    h.key_press(Key::Enter);
    until(&mut h, "the refusal", |h| {
        state(h).confirm.is_none() && shown(h, "not deleted: type delete to confirm")
    });
    assert!(big.exists());

    // Switching the method clears the word.
    h.event(egui::Event::Text("D".into()));
    until(&mut h, "the dialog", |h| shown(h, word));
    settle(&mut h);
    h.get_by_label(word).click();
    h.step();
    h.get_by_label(word).type_text("delete");
    until(&mut h, "the word", |h| !disabled(h, "Delete"));
    settle(&mut h);
    h.get_by_label("Trash, recoverable").click();
    until(&mut h, "the Trash", |h| {
        shown(h, "Move big.bin to the Trash?")
    });
    settle(&mut h);
    h.get_by_label("Delete permanently").click();
    until(&mut h, "the word cleared", |h| {
        shown(h, "Permanently delete big.bin?") && disabled(h, "Delete")
    });

    // With the word, it deletes.
    settle(&mut h);
    h.get_by_label(word).click();
    h.step();
    h.get_by_label(word).type_text("delete");
    until(&mut h, "the word", |h| !disabled(h, "Delete"));
    settle(&mut h);
    h.get_by_label("Delete").click();
    until(&mut h, "the delete", |h| {
        !big.exists() && !shown(h, "big.bin")
    });
    close(h);
}
