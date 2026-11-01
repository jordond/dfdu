//! The dialog drawn, and the keys that turn into its intents; the gate itself is tested in
//! `dfdu_session::confirm`.

use std::time::Instant;

use dfdu_engine::Command;
use dfdu_tree::HostPath;

use super::*;
use crate::model::snapshot::{EntryData, Mark, Snapshot, test_entry};
use crate::tests::{press, render, snap_without_totals};
use crate::theme::Theme;

fn dialog(app: &App) -> &Confirm {
    app.session.confirm.as_ref().expect("the dialog is open")
}

fn type_word(app: &mut App, t: Instant) {
    for c in WORD.chars() {
        assert!(app.key(press(KeyCode::Char(c)), t).is_empty());
    }
}

/// Twelve entries of 1 to 12 MiB, all marked.
fn twelve_marked(t: Instant) -> App {
    let mut app = App::new(Theme::no_color(), t);
    let es = (0..12)
        .map(|i| test_entry(i, &format!("e{i:02}"), (u64::from(i) + 1) << 20))
        .collect();
    app.refresh(snap_without_totals(es, 1), t);
    for _ in 0..12 {
        app.key(press(KeyCode::Char(' ')), t);
    }
    assert_eq!(app.session.marks.items().len(), 12);
    app
}

#[test]
fn trash_delete_sends_a_plan_after_y() {
    let t0 = Instant::now();
    let mut app = App::new(Theme::no_color(), t0);
    app.refresh(snap_without_totals(vec![test_entry(0, "a", 10)], 1), t0);
    let fx = app.key(press(KeyCode::Char('d')), t0);
    assert!(
        matches!(
            fx.as_slice(),
            [Effect::Send(Command::Estimate { id: 1, items })] if items.len() == 1
        ),
        "the dialog asks what the delete frees: {fx:?}"
    );
    assert_eq!(
        (dialog(&app).mode(), dialog(&app).typing()),
        (Mode::Trash, false)
    );
    let fx = app.key(press(KeyCode::Char('y')), t0);
    assert!(matches!(
        fx.as_slice(),
        [Effect::Send(Command::Delete {
            mode: Mode::Trash,
            ..
        })]
    ));
    assert!(app.session.confirm.is_none());
}

#[test]
fn with_the_word_asked_for_the_keys_type_it() {
    let t0 = Instant::now();
    let mut app = App::new(Theme::no_color(), t0);
    app.refresh(snap_without_totals(vec![test_entry(0, "a", 10)], 1), t0);
    app.key(press(KeyCode::Char('D')), t0);
    // 'y' is typed, and Backspace takes it back.
    assert!(app.key(press(KeyCode::Char('y')), t0).is_empty());
    assert_eq!(dialog(&app).typed().text(), "y");
    // 's' switches the method only before anything is typed.
    app.key(press(KeyCode::Char('s')), t0);
    assert_eq!(dialog(&app).typed().text(), "ys");
    app.key(press(KeyCode::Backspace), t0);
    app.key(press(KeyCode::Backspace), t0);
    assert!(app.key(press(KeyCode::Up), t0).is_empty(), "no text op");
    assert!(app.session.confirm.is_some());
    type_word(&mut app, t0);
    let fx = app.key(press(KeyCode::Enter), t0);
    assert!(matches!(
        fx.as_slice(),
        [Effect::Send(Command::Delete {
            mode: Mode::Permanent,
            ..
        })]
    ));
    // Without the word asked for, a key other than 'y', 'p' and 's' cancels.
    app.key(press(KeyCode::Char('d')), t0);
    assert!(app.key(press(KeyCode::Enter), t0).is_empty());
    assert!(app.session.confirm.is_none());
}

#[test]
fn dialog_lists_targets_and_more() {
    let t = Instant::now();
    let mut app = twelve_marked(t);
    app.key(press(KeyCode::Char('d')), t);
    let out = render(&mut app, t, 100, 30);
    assert!(out.contains("Move 12 entries to the Trash?"), "{out}");
    // The eight largest are listed, largest first.
    let listed: Vec<usize> = (0..12)
        .filter_map(|i| out.find(&format!("e{i:02}")))
        .collect();
    assert_eq!(listed.len(), 8, "{out}");
    for i in 4..12 {
        assert!(
            out.contains(&format!("{}  e{i:02}", human((i + 1) << 20))),
            "{out}"
        );
    }
    assert!(out.find("e11").unwrap() < out.find("e04").unwrap(), "{out}");
    assert!(out.contains("and 4 more"), "{out}");
    assert!(
        out.contains(&format!("{}  total · 12 items", human(78 << 20))),
        "{out}"
    );
    assert!(out.contains("Method: Trash, recoverable"), "{out}");
}

#[test]
fn shows_the_engine_estimate() {
    let t = Instant::now();
    let mut app = App::new(Theme::no_color(), t);
    app.refresh(snap_without_totals(vec![test_entry(0, "a", 10)], 1), t);
    app.key(press(KeyCode::Char('d')), t);
    let mut s = snap_without_totals(vec![test_entry(0, "a", 10)], 2);
    s.estimate = Some(dfdu_engine::Estimate {
        id: 1,
        bytes: 3 << 20,
        linked_out: 1 << 20,
        ..Default::default()
    });
    app.refresh(s, t);
    let out = render(&mut app, t, 100, 30);
    assert!(
        out.contains("Frees about 3.0 MiB; stays: 1.0 MiB hard-linked from outside"),
        "{out}"
    );
}

#[test]
fn s_switches_to_staging() {
    let t = Instant::now();
    let mut app = App::new(Theme::no_color(), t);
    app.refresh(snap_without_totals(vec![test_entry(0, "a", 10)], 1), t);
    app.key(press(KeyCode::Char('d')), t);
    assert!(app.key(press(KeyCode::Char('s')), t).is_empty());
    assert_eq!(
        (dialog(&app).mode(), dialog(&app).typing()),
        (Mode::Stage, false)
    );
    let out = render(&mut app, t, 100, 30);
    assert!(out.contains("Stage a for deletion?"), "{out}");
    assert!(out.contains("U puts it back"), "{out}");
    assert!(out.contains("s the Trash"), "{out}");
    let fx = app.key(press(KeyCode::Char('y')), t);
    assert!(
        matches!(
            fx.as_slice(),
            [Effect::Send(Command::Delete {
                mode: Mode::Stage,
                ..
            })]
        ),
        "{fx:?}"
    );
}

#[test]
fn p_switches_method() {
    let t = Instant::now();
    let mut app = App::new(Theme::no_color(), t);
    app.refresh(snap_without_totals(vec![test_entry(0, "a", 10)], 1), t);
    app.key(press(KeyCode::Char('d')), t);
    assert!(app.key(press(KeyCode::Char('p')), t).is_empty());
    assert_eq!(
        (dialog(&app).mode(), dialog(&app).typing()),
        (Mode::Permanent, true)
    );
    let out = render(&mut app, t, 100, 30);
    assert!(out.contains("Permanently delete a?"), "{out}");
    assert!(out.contains("p move to the Trash  s stage"), "{out}");
    // Permanent now: 'y' is only typed.
    assert!(app.key(press(KeyCode::Char('y')), t).is_empty());
    assert!(app.session.confirm.is_some());
    // Back to the Trash: the typed text is dropped and 'y' confirms again.
    app.key(press(KeyCode::Char('p')), t);
    assert_eq!(
        (dialog(&app).mode(), dialog(&app).typing()),
        (Mode::Trash, false)
    );
    assert!(dialog(&app).typed().is_empty());
    let fx = app.key(press(KeyCode::Char('y')), t);
    assert!(matches!(
        fx.as_slice(),
        [Effect::Send(Command::Delete {
            mode: Mode::Trash,
            ..
        })]
    ));
    // From D, p goes to the Trash.
    app.key(press(KeyCode::Char('D')), t);
    app.key(press(KeyCode::Char('p')), t);
    assert_eq!(dialog(&app).mode(), Mode::Trash);
    assert!(app.key(press(KeyCode::Char('n')), t).is_empty());
    assert!(app.session.confirm.is_none());
}

#[test]
fn large_trash_delete_needs_typing() {
    let t = Instant::now();
    let gib = 1 << 30;
    let mut app = App::new(Theme::no_color(), t);
    app.refresh(
        snap_without_totals(
            vec![
                test_entry(0, "big", 50 * gib),
                test_entry(1, "less", 49 * gib),
            ],
            1,
        ),
        t,
    );
    app.key(press(KeyCode::Char('d')), t);
    assert!(dialog(&app).typing());
    let out = render(&mut app, t, 100, 30);
    assert!(out.contains("Move big to the Trash?"), "{out}");
    assert!(
        out.contains("50.0 GiB or more: confirm by typing the word."),
        "{out}"
    );
    assert!(out.contains("type delete and Enter"), "{out}");
}

#[test]
fn trash_delete_while_scanning_needs_typing() {
    let t = Instant::now();
    let mut app = App::new(Theme::no_color(), t);
    let scanning = EntryData {
        is_dir: true,
        mark: Mark::Scanning,
        ..test_entry(0, "growing", 40 << 30)
    };
    app.refresh(
        snap_without_totals(vec![scanning, test_entry(1, "done", 10)], 1),
        t,
    );
    app.key(press(KeyCode::Char('d')), t);
    assert!(dialog(&app).typing());
    let out = render(&mut app, t, 100, 30);
    assert!(out.contains("at least 40.0 GiB (still scanning)"), "{out}");
    assert!(
        out.contains("Still scanning, may be 50.0 GiB or more: confirm by typing the word."),
        "{out}"
    );
    app.key(press(KeyCode::Esc), t);
    // Marked together with a finished entry, it still needs the word; the finished one
    // alone does not.
    app.key(press(KeyCode::Char(' ')), t);
    app.key(press(KeyCode::Char(' ')), t);
    app.key(press(KeyCode::Char('d')), t);
    assert!(dialog(&app).typing());
    let out = render(&mut app, t, 100, 30);
    assert!(out.contains("Move 2 entries to the Trash?"), "{out}");
    assert!(out.contains("Still scanning, may be"), "{out}");
    app.key(press(KeyCode::Esc), t);
    crate::state::marks::clear(&mut app);
    app.key(press(KeyCode::Char('d')), t);
    assert!(!dialog(&app).typing());
}

#[test]
fn one_command_with_all_outermost_targets() {
    let t0 = Instant::now();
    let mut app = App::new(Theme::no_color(), t0);
    let a = EntryData {
        is_dir: true,
        ..test_entry(0, "a", 5000)
    };
    let (b, c) = (test_entry(1, "b", 300), test_entry(2, "c", 200));
    app.refresh(
        snap_without_totals(vec![a.clone(), b.clone(), c.clone()], 1),
        t0,
    );
    app.key(press(KeyCode::Char(' ')), t0);
    app.key(press(KeyCode::Char(' ')), t0);
    // Into a/, and mark x there.
    app.key(press(KeyCode::Up), t0);
    app.key(press(KeyCode::Up), t0);
    app.key(press(KeyCode::Enter), t0);
    let x = test_entry(3, "x", 4000);
    app.refresh(
        Snapshot {
            dir_path: HostPath::from("/r").join(b"a"),
            dir_node: Some(a.node),
            entries: vec![x.clone()],
            version: 2,
            ..Default::default()
        },
        t0,
    );
    app.key(press(KeyCode::Char(' ')), t0);
    app.key(press(KeyCode::Char('d')), t0);
    let out = render(&mut app, t0, 100, 30);
    assert!(out.contains("Move 2 entries to the Trash?"), "{out}");
    // Seen from a/, the others are shown by their full path.
    assert!(out.contains("/r/a/"), "{out}");
    assert!(out.contains("/r/b"), "{out}");
    assert!(out.contains("x  inside another marked item"), "{out}");
    assert!(
        out.contains(&format!("{}  total · 2 items", human(5300))),
        "{out}"
    );
    let fx = app.key(press(KeyCode::Char('y')), t0);
    let [Effect::Send(Command::Delete { items, mode })] = fx.as_slice() else {
        panic!("expected one delete: {fx:?}");
    };
    assert_eq!(
        (items.as_slice(), *mode),
        ([a.node, b.node].as_slice(), Mode::Trash)
    );
}

#[test]
fn dialog_fits_a_small_terminal() {
    let t = Instant::now();
    let mut app = twelve_marked(t);
    for k in ['d', 'D'] {
        app.key(press(KeyCode::Char(k)), t);
        // Smaller than the dialog: the dialog is clamped to the screen.
        let out = render(&mut app, t, 40, 8);
        assert_eq!(out.lines().count(), 8, "{out}");
        assert!(out.contains('┌'), "{out}");
        let out = render(&mut app, t, 100, 30);
        assert!(out.contains('┌') && out.contains('┘'), "{out}");
        assert!(out.contains("and 4 more"), "{out}");
        app.key(press(KeyCode::Esc), t);
        assert!(app.session.confirm.is_none());
    }
}
