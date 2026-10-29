//! A read-only browser (`--read-only`, imported scans) offers no deletion: the keys that delete
//! only say so, and the footer, the help and the palette leave them out.

use std::time::Instant;

use ratatui::crossterm::event::KeyCode;

use super::{library_snap, press, render};
use crate::app::keys::{ACTIONS, Action, READ_ONLY, offered};
use crate::app::{App, Overlay};
use crate::theme::Theme;

fn app(read_only: bool) -> (App, Instant) {
    let t0 = Instant::now();
    let mut app = App::new(Theme::no_color(), t0);
    app.read_only = read_only;
    app.refresh(library_snap(), t0);
    (app, t0)
}

#[test]
fn delete_keys_open_no_dialog() {
    let (mut ro, t0) = app(true);
    ro.key(press(KeyCode::Char(' ')), t0);
    assert!(!ro.marks.is_empty());
    for c in ['d', 'D', 'U'] {
        ro.status = None;
        let fx = ro.key(press(KeyCode::Char(c)), t0);
        assert!(fx.is_empty(), "{c}: {fx:?}");
        assert!(matches!(ro.overlay, Overlay::None), "{c}");
        assert_eq!(ro.status.as_deref(), Some(READ_ONLY), "{c}");
    }
    // The same key opens the dialog in a browser that deletes.
    let (mut rw, t0) = app(false);
    rw.key(press(KeyCode::Char('d')), t0);
    assert!(matches!(rw.overlay, Overlay::ConfirmDelete(_)));
}

#[test]
fn header_and_footer_show_it() {
    let (mut ro, t0) = app(true);
    ro.key(press(KeyCode::Char(' ')), t0);
    let frame = render(&mut ro, t0, 120, 8);
    let header = frame.lines().next().unwrap_or_default();
    assert!(header.contains("read-only"), "{frame}");
    assert!(!frame.contains("trash marked"), "{frame}");
    assert!(!frame.contains("delete marked"), "{frame}");

    let (mut rw, t0) = app(false);
    rw.key(press(KeyCode::Char(' ')), t0);
    let frame = render(&mut rw, t0, 120, 8);
    assert!(!frame.contains("read-only"), "{frame}");
    assert!(frame.contains("trash marked"), "{frame}");
}

#[test]
fn help_and_palette_leave_out_deletion() {
    let deleting = [Action::Trash, Action::DeletePermanent, Action::Undo];
    let shown: Vec<Action> = offered(true).map(|i| i.action).collect();
    assert_eq!(shown.len(), ACTIONS.len() - deleting.len());
    assert!(deleting.iter().all(|a| a.deletes() && !shown.contains(a)));
    assert_eq!(offered(false).count(), ACTIONS.len());

    let (mut ro, t0) = app(true);
    ro.key(press(KeyCode::Char('?')), t0);
    let help = render(&mut ro, t0, 100, 60);
    assert!(help.contains("rescan the selected directory"), "{help}");
    assert!(!help.contains("move to Trash"), "{help}");
    assert!(!help.contains("delete permanently"), "{help}");
    ro.key(press(KeyCode::Esc), t0);

    ro.key(press(KeyCode::Char(':')), t0);
    for c in "trash".chars() {
        ro.key(press(KeyCode::Char(c)), t0);
    }
    let palette = render(&mut ro, t0, 100, 30);
    assert!(!palette.contains("move to Trash"), "{palette}");
    ro.key(press(KeyCode::Enter), t0);
    assert!(!matches!(ro.overlay, Overlay::ConfirmDelete(_)));
}
