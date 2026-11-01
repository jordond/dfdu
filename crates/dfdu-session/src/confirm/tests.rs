use dfdu_engine::{Command, Mode};
use dfdu_tree::filetype::TypeFilter;

use super::*;
use crate::snapshot::{EntryData, Mark, test_entry};
use crate::state::tests::browser;

const GIB: u64 = 1 << 30;

fn dialog(s: &SessionState) -> &Confirm {
    s.confirm.as_ref().expect("the dialog is open")
}

/// Types `text` into the dialog; nothing is sent.
fn type_text(s: &mut SessionState, text: &str) {
    for c in text.chars() {
        assert!(act(s, Intent::Text(TextOp::Insert(c))).effects.is_empty());
    }
}

/// The method of the one delete in `then`; `None` when it sends none.
fn deleted(then: &Then) -> Option<Mode> {
    match then.effects.as_slice() {
        [Effect::Send(Command::Delete { mode, .. })] => Some(*mode),
        [] => None,
        other => panic!("more than one delete: {other:?}"),
    }
}

#[test]
fn a_trash_delete_asks_what_it_frees_then_sends_the_delete() {
    let mut s = browser(vec![test_entry(0, "a", 10)]);
    let fx = open(&mut s, Mode::Trash);
    assert!(
        matches!(
            fx.as_slice(),
            [Effect::Send(Command::Estimate { id: 1, items })] if items.len() == 1
        ),
        "{fx:?}"
    );
    assert_eq!(
        (dialog(&s).mode(), dialog(&s).typing()),
        (Mode::Trash, false)
    );
    assert_eq!(dialog(&s).estimate(), 1);
    assert_eq!(deleted(&act(&mut s, Intent::Confirm)), Some(Mode::Trash));
    assert!(s.confirm.is_none());
    assert_eq!(s.status.as_deref(), Some("moving to the Trash…"));
}

#[test]
fn a_permanent_delete_needs_the_word_typed() {
    let mut s = browser(vec![test_entry(0, "a", 10)]);
    let _ = open(&mut s, Mode::Permanent);
    assert!(dialog(&s).typing());
    type_text(&mut s, "delet");
    let then = act(&mut s, Intent::Text(TextOp::Submit));
    assert_eq!(deleted(&then), None, "an incomplete word does not delete");
    assert!(s.confirm.is_none());
    assert_eq!(
        s.status.as_deref(),
        Some("not deleted: type delete to confirm")
    );
    let _ = open(&mut s, Mode::Permanent);
    assert_eq!(
        deleted(&act(&mut s, Intent::Confirm)),
        None,
        "nothing typed"
    );
    let _ = open(&mut s, Mode::Permanent);
    type_text(&mut s, WORD);
    assert_eq!(
        deleted(&act(&mut s, Intent::Text(TextOp::Submit))),
        Some(Mode::Permanent)
    );
}

#[test]
fn the_word_takes_sixteen_characters_at_most() {
    let mut s = browser(vec![test_entry(0, "a", 10)]);
    let _ = open(&mut s, Mode::Permanent);
    type_text(&mut s, &"x".repeat(20));
    assert_eq!(dialog(&s).typed().text().chars().count(), TYPED_MAX);
    let _ = act(&mut s, Intent::Text(TextOp::Clear));
    type_text(&mut s, WORD);
    assert_eq!(
        deleted(&act(&mut s, Intent::Confirm)),
        Some(Mode::Permanent)
    );
}

#[test]
fn switching_the_method_drops_the_typed_text() {
    let mut s = browser(vec![test_entry(0, "a", 10)]);
    let _ = open(&mut s, Mode::Trash);
    let _ = act(&mut s, Intent::SwitchStage);
    assert_eq!(
        (dialog(&s).mode(), dialog(&s).typing()),
        (Mode::Stage, false)
    );
    let _ = act(&mut s, Intent::SwitchPermanent);
    assert_eq!(
        (dialog(&s).mode(), dialog(&s).typing()),
        (Mode::Permanent, true)
    );
    type_text(&mut s, "del");
    let _ = act(&mut s, Intent::SwitchPermanent);
    assert_eq!(
        (dialog(&s).mode(), dialog(&s).typing()),
        (Mode::Trash, false)
    );
    assert!(dialog(&s).typed().is_empty());
    let _ = act(&mut s, Intent::SwitchStage);
    let _ = act(&mut s, Intent::SwitchStage);
    assert_eq!(dialog(&s).mode(), Mode::Trash, "staging and back");
    // Text is typed only where the word is asked for.
    type_text(&mut s, "y");
    assert!(dialog(&s).typed().is_empty());
    assert_eq!(deleted(&act(&mut s, Intent::Cancel)), None);
    assert!(s.confirm.is_none());
}

#[test]
fn a_large_or_unfinished_trash_delete_needs_the_word() {
    let mut s = browser(vec![
        test_entry(0, "big", 50 * GIB),
        test_entry(1, "less", 49 * GIB),
    ]);
    let _ = open(&mut s, Mode::Trash);
    assert!(dialog(&s).typing());
    assert_eq!(deleted(&act(&mut s, Intent::Confirm)), None);
    s.move_by(1);
    let _ = open(&mut s, Mode::Trash);
    assert!(!dialog(&s).typing(), "just under the threshold");
    let _ = act(&mut s, Intent::Cancel);
    // Marked entries count together.
    crate::marks::toggle(&mut s);
    s.move_to(0);
    crate::marks::toggle(&mut s);
    let _ = open(&mut s, Mode::Trash);
    assert!(dialog(&s).typing());
    assert_eq!(dialog(&s).total(), 99 * GIB);

    let scanning = EntryData {
        is_dir: true,
        mark: Mark::Scanning,
        ..test_entry(0, "growing", 40 * GIB)
    };
    let mut s = browser(vec![scanning, test_entry(1, "done", 10)]);
    let _ = open(&mut s, Mode::Trash);
    assert!(dialog(&s).typing() && dialog(&s).scanning());
    type_text(&mut s, WORD);
    assert_eq!(deleted(&act(&mut s, Intent::Confirm)), Some(Mode::Trash));
    s.move_to(1);
    let _ = open(&mut s, Mode::Trash);
    assert!(!dialog(&s).typing(), "the finished one alone");
}

#[test]
fn a_read_only_browser_opens_no_dialog() {
    let mut s = browser(vec![test_entry(0, "a", 10)]);
    crate::marks::toggle(&mut s);
    s.read_only = true;
    for mode in [Mode::Trash, Mode::Stage, Mode::Permanent] {
        s.status = None;
        assert!(open(&mut s, mode).is_empty());
        assert!(s.confirm.is_none());
        assert_eq!(s.status.as_deref(), Some(READ_ONLY));
    }
    let only = s
        .marks
        .items()
        .iter()
        .filter_map(|m| m.target.node)
        .collect();
    assert!(open_only(&mut s, Mode::Trash, &only).is_empty());
    assert!(s.confirm.is_none());
}

#[test]
fn rows_that_are_no_entries_and_folders_cut_by_types_are_refused() {
    let shared = EntryData {
        pseudo: true,
        ..test_entry(1, "shared", 5)
    };
    let mut s = browser(vec![shared]);
    assert!(open(&mut s, Mode::Trash).is_empty());
    assert!(s.confirm.is_none());
    let dir = EntryData {
        is_dir: true,
        ..test_entry(0, "d", 10)
    };
    let mut s = browser(vec![dir]);
    s.types = TypeFilter::parse("video").expect("types");
    assert!(open(&mut s, Mode::Trash).is_empty());
    assert!(s.confirm.is_none());
    assert_eq!(s.status.as_deref(), Some(NO_FOLDERS));
}

#[test]
fn only_the_marks_a_review_view_lists_are_sent() {
    let mut s = browser(
        (0..3)
            .map(|i| test_entry(i, &format!("f{i}"), 10))
            .collect(),
    );
    for _ in 0..3 {
        crate::marks::toggle(&mut s);
    }
    let first = s.marks.items()[0].target.node.expect("a node");
    let _ = open_only(&mut s, Mode::Trash, &HashSet::from([first]));
    let then = act(&mut s, Intent::Confirm);
    let [Effect::Send(Command::Delete { items, .. })] = then.effects.as_slice() else {
        panic!("expected one delete: {:?}", then.effects);
    };
    assert_eq!(items.as_slice(), [first]);
    assert_eq!(
        s.marks.items().len(),
        3,
        "the marks stay until the delete is done"
    );
}
