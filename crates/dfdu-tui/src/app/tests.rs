use super::*;
use crate::model::snapshot::test_entry;
use crate::tests::{press, snap_without_totals};

#[test]
fn sort_keys_apply_without_a_new_snapshot() {
    let t0 = Instant::now();
    let mut app = App::new(Theme::no_color(), t0);
    let es = vec![
        test_entry(0, "b", 30),
        test_entry(1, "c", 20),
        test_entry(2, "a", 10),
    ];
    app.refresh(snap_without_totals(es, 1), t0);
    app.key(press(KeyCode::Down), t0);
    let picked = app.selected.as_ref().and_then(|s| s.node);
    app.key(press(KeyCode::Char('n')), t0);
    app.reorder(t0);
    let names: Vec<&[u8]> = app
        .rows
        .iter()
        .map(|&i| &app.snap.entries[i].name[..])
        .collect();
    assert_eq!(names, [b"a", b"b", b"c"]);
    assert_eq!(app.selected.as_ref().and_then(|s| s.node), picked);
    assert_eq!(app.selected_index(), Some(2));
}
