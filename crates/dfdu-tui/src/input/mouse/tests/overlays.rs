//! Clicks over what covers the list: the debug panel takes none, a click outside an overlay
//! closes it, and the delete dialog ignores the mouse.

use dfdu_engine::ErrorList;

use super::*;

#[test]
fn clicks_and_wheel_over_the_debug_panel_change_nothing() {
    let t0 = Instant::now();
    let mut app = browser(t0);
    crate::features::debug::toggle(&mut app);
    render(&mut app, t0, 100, 20);
    let panel = app.areas.debug.expect("debug panel");
    let (x, y) = (panel.x + 2, TOP + 1);
    assert!(panel.contains(Position::new(x, y)));
    for kind in [
        MouseEventKind::Down(MouseButton::Left),
        MouseEventKind::Down(MouseButton::Left),
        MouseEventKind::ScrollDown,
        MouseEventKind::ScrollUp,
    ] {
        assert!(event(&mut app, mouse(kind, x, y), t0).is_none(), "{kind:?}");
    }
    assert_eq!(selected(&app).as_deref(), Some("d0"));
    assert_eq!(app.target.path, HostPath::from(""));
    // Left of the panel, the same row takes the click.
    event(&mut app, left(panel.x - 1, y), t0).expect("redraw");
    assert_eq!(selected(&app).as_deref(), Some("d1"));
}

#[test]
fn click_outside_overlay_closes_it() {
    let t0 = Instant::now();
    let mut app = browser(t0);
    app.show_errors(ErrorList::default());
    render(&mut app, t0, 80, 12);
    let r = app.areas.overlay.expect("the overlay's area");
    assert!(event(&mut app, left(r.x + 1, r.y + 1), t0).is_none());
    assert!(matches!(app.overlay, Overlay::Errors { .. }));
    let before = selected(&app);
    event(&mut app, left(r.x.saturating_sub(1), TOP + 3), t0).expect("redraw");
    assert!(matches!(app.overlay, Overlay::None));
    assert_eq!(selected(&app), before, "the click only closed the overlay");
}

#[test]
fn click_never_touches_the_delete_dialog() {
    let t0 = Instant::now();
    let mut app = browser(t0);
    app.key(press(KeyCode::Char('D')), t0);
    render(&mut app, t0, 80, 12);
    assert!(matches!(app.overlay, Overlay::ConfirmDelete(_)));
    let r = app.areas.overlay.expect("the dialog's area");
    let wheel = mouse(MouseEventKind::ScrollDown, 0, TOP);
    for m in [left(0, 0), left(0, TOP + 5), left(r.x + 1, r.y + 1), wheel] {
        assert!(event(&mut app, m, t0).is_none());
        assert!(matches!(app.overlay, Overlay::ConfirmDelete(_)));
    }
    assert_eq!(selected(&app).as_deref(), Some("d0"));
}
