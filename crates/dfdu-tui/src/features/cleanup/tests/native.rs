//! The cleanup commands of the items' own tools: a dry run first, then the command, and only
//! the dry run in a read-only browser.

use std::time::Instant;

use ratatui::crossterm::event::KeyCode;

use super::super::*;
use super::opened;
use crate::tests::{classified, sync};
use crate::tests::{press, render};
use crate::theme::Theme;

#[test]
fn native_commands_start_with_their_dry_run() {
    let fx = classified();
    let t0 = Instant::now();
    let mut app = opened(&fx.m, t0);
    // The regenerable group holds `big`, a Rust target/ cleaned with `cargo clean`.
    let groups = rows(&app);
    let at = groups
        .iter()
        .position(|r| {
            matches!(
                r,
                Row::Group {
                    safety: Safety::Regenerable,
                    ..
                }
            )
        })
        .unwrap();
    app.cleanup.cursor = at;
    app.key(press(KeyCode::Enter), t0);
    app.key(press(KeyCode::Down), t0);
    let out = app.key(press(KeyCode::Char('N')), t0);
    assert!(
        matches!(
            out.as_slice(),
            [Effect::Send(Command::RunNative {
                dry_run: true,
                variant: None,
                ..
            })]
        ),
        "{out:?}"
    );
    // While it runs, `y` does nothing.
    assert!(app.key(press(KeyCode::Char('y')), t0).is_empty());
    let id = app.cleanup.pane.as_ref().unwrap().id;
    app.extras.cleanup.native = Some(NativeView {
        id,
        command: "cargo clean --dry-run".into(),
        lines: vec!["Summary 3 files".into()],
        done: Some(Ok(())),
    });
    let frame = render(&mut app, t0, 100, 24);
    assert!(frame.contains("That was the dry run"), "{frame}");
    let out = app.key(press(KeyCode::Char('y')), t0);
    assert!(
        matches!(
            out.as_slice(),
            [Effect::Send(Command::RunNative { dry_run: false, .. })]
        ),
        "{out:?}"
    );
}

/// A read-only browser runs a native command's dry run, but not the command.
#[test]
fn read_only_runs_only_the_dry_run() {
    let fx = classified();
    let t0 = Instant::now();
    let mut app = App::new(Theme::no_color(), t0);
    app.read_only = true;
    sync(&mut app, &fx.m, t0);
    app.key(press(KeyCode::Char('3')), t0);
    sync(&mut app, &fx.m, t0);
    let at = rows(&app)
        .iter()
        .position(|r| {
            matches!(
                r,
                Row::Group {
                    safety: Safety::Regenerable,
                    ..
                }
            )
        })
        .unwrap();
    app.cleanup.cursor = at;
    app.key(press(KeyCode::Enter), t0);
    app.key(press(KeyCode::Down), t0);
    let out = app.key(press(KeyCode::Char('N')), t0);
    assert!(
        matches!(
            out.as_slice(),
            [Effect::Send(Command::RunNative { dry_run: true, .. })]
        ),
        "{out:?}"
    );
    let id = app.cleanup.pane.as_ref().unwrap().id;
    app.extras.cleanup.native = Some(NativeView {
        id,
        command: "cargo clean --dry-run".into(),
        lines: vec!["Summary 3 files".into()],
        done: Some(Ok(())),
    });
    let frame = render(&mut app, t0, 100, 24);
    assert!(
        frame.contains("read-only mode runs nothing more"),
        "{frame}"
    );
    assert!(!frame.contains("y run it"), "{frame}");
    assert!(app.key(press(KeyCode::Char('y')), t0).is_empty());
    assert_eq!(app.status.as_deref(), Some(crate::app::keys::READ_ONLY));
}
