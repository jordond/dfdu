use ratatui::crossterm::event::KeyCode;
use ratatui::style::Modifier;

use super::*;
use crate::tests::{key, style_at, text};

#[test]
fn deleted_row_stays_as_placeholder_until_resort() {
    let t0 = Instant::now();
    let mut app = animated(Theme::no_color(), t0);
    let (a, c) = (test_entry(0, "a", 300), test_entry(2, "c", 100));
    let b = EntryData {
        mark: Mark::Deleting,
        ..test_entry(1, "b", 200)
    };
    app.refresh(snap(vec![a.clone(), b, c.clone()], 1), t0);
    render_buffer(&mut app, t0, W, H);

    let t1 = t0 + ms(100);
    app.refresh(snap(vec![a.clone(), c.clone()], 2), t1);
    assert_eq!(names(&app), ["a", "b", "c"]);
    assert!(app.snap.entries[app.rows[1]].ghost);
    let buf = render_buffer(&mut app, t1, W, H);
    let frame = text(&buf);
    assert!(frame.contains("b  deleted"), "{frame}");
    let at = style_at(&buf, "b  deleted");
    assert!(at.add_modifier.contains(Modifier::CROSSED_OUT), "{at:?}");
    // Kept while the snapshots go on without it.
    let t2 = t0 + ms(200);
    app.refresh(snap(vec![a.clone(), c.clone()], 3), t2);
    assert_eq!(names(&app), ["a", "b", "c"]);

    app.key(key(KeyCode::Char('S')), t2);
    app.reorder(t2);
    assert_eq!(names(&app), ["a", "c"]);
    app.refresh(snap(vec![a, c], 4), t0 + ms(300));
    assert_eq!(names(&app), ["a", "c"]);
    assert_eq!(app.snap.entries.len(), 2, "the ghost was added again");
}

#[test]
fn entry_gone_while_a_delete_ran_becomes_a_placeholder() {
    let t0 = Instant::now();
    let mut app = animated(Theme::no_color(), t0);
    let es = vec![test_entry(0, "a", 300), test_entry(1, "b", 200)];
    let mut before = snap(es.clone(), 1);
    before.deleting = Some((0, 1));
    app.refresh(before, t0);
    // Deleted before any snapshot showed it marked.
    app.refresh(snap(vec![es[0].clone()], 2), t0 + ms(100));
    assert_eq!(names(&app), ["a", "b"]);
    // Gone without a delete: no placeholder.
    let t1 = t0 + ms(200);
    app.key(key(KeyCode::Char('S')), t1);
    app.reorder(t1);
    app.refresh(snap(es.clone(), 3), t1);
    app.refresh(snap(vec![es[0].clone()], 4), t1 + ms(100));
    assert_eq!(names(&app), ["a"]);
}
