//! The review views as a whole: every key on an empty view, and `d` acting on what the view
//! ticked, from a key or from the palette.

use std::time::Instant;

use dfdu_engine::Command;
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::app::keys::{self, ACTIONS, Action};
use crate::app::views::{self, View};
use crate::app::{App, Effect, Overlay};
use crate::features::treemap::tests::{Fixture, sync};
use crate::tests::{press, render};
use crate::theme::Theme;

/// `/r` with one empty directory, classified with the built-in rules: no matches, no files.
fn empty() -> Fixture {
    let (mut fx, root) = Fixture::new();
    fx.dir(root, "empty");
    fx.m.tree.dir_mut(root).expect("root").state = dfdu_tree::DirState::Complete;
    let env = dfdu_rules::Env::with(dfdu_rules::Platform::Linux, &[("HOME", "/home/u")]);
    fx.m.rules = Some(std::sync::Arc::new(dfdu_rules::builtin(&env).0));
    fx
}

/// Every key of the registry and the arrows, as a key event.
fn every_key() -> Vec<KeyEvent> {
    let mut out: Vec<KeyEvent> = [
        KeyCode::Left,
        KeyCode::Right,
        KeyCode::Up,
        KeyCode::Down,
        KeyCode::Enter,
        KeyCode::Esc,
        KeyCode::Home,
        KeyCode::End,
        KeyCode::PageUp,
        KeyCode::PageDown,
        KeyCode::Backspace,
        KeyCode::Char(' '),
        KeyCode::Char('h'),
        KeyCode::Char('l'),
    ]
    .into_iter()
    .map(press)
    .collect();
    for info in ACTIONS {
        for key in info.keys.split_whitespace() {
            let (code, ctrl) = keys::parse(key).expect("a key");
            let mods = if ctrl {
                KeyModifiers::CONTROL
            } else {
                KeyModifiers::NONE
            };
            out.push(KeyEvent::new(code, mods));
        }
    }
    out
}

/// No key panics on Cleanup, Duplicates or Largest with nothing to list.
#[test]
fn every_key_on_an_empty_review_view_is_safe() {
    let fx = empty();
    let t0 = Instant::now();
    for view in [View::Cleanup, View::Duplicates, View::Largest] {
        for k in every_key() {
            let mut app = App::new(Theme::no_color(), t0);
            app.outside.local = true;
            sync(&mut app, &fx.m, t0);
            views::switch(&mut app, view);
            sync(&mut app, &fx.m, t0);
            render(&mut app, t0, 80, 20);
            assert_eq!(views::current(&app), view);
            app.key(k, t0);
            render(&mut app, t0, 80, 20);
        }
    }
}

/// `/r` with `big` (two files) and `small`, a file, open in Largest.
fn largest() -> (App, Fixture, Instant) {
    let (mut fx, root) = Fixture::new();
    let big = fx.dir(root, "big");
    fx.file(big, "a.iso", 9000);
    fx.file(big, "b.iso", 8000);
    fx.file(root, "small", 100);
    fx.m.tree.dir_mut(root).expect("root").state = dfdu_tree::DirState::Complete;
    let t0 = Instant::now();
    let mut app = App::new(Theme::no_color(), t0);
    sync(&mut app, &fx.m, t0);
    (app, fx, t0)
}

/// The nodes a delete dialog's estimate was asked for.
fn asked(fx: &[Effect]) -> usize {
    match fx {
        [Effect::Send(Command::Estimate { items, .. })] => items.len(),
        _ => panic!("no estimate: {fx:?}"),
    }
}

/// With nothing ticked in the view, `d` from the palette deletes nothing either: not the
/// list's selection hidden behind the view.
#[test]
fn the_palettes_delete_needs_ticks_in_a_review_view() {
    let (mut app, fx, t0) = largest();
    views::switch(&mut app, View::Largest);
    sync(&mut app, &fx.m, t0);
    for mode in [Action::Trash, Action::DeletePermanent] {
        assert!(keys::run(&mut app, mode, t0).is_empty());
        assert!(matches!(app.overlay, Overlay::None));
        let status = app.status.clone().unwrap_or_default();
        assert!(status.contains("tick items with Space first"), "{status}");
    }
}

/// `d` in a review view deletes what the view ticked, never marks made in Files.
#[test]
fn delete_in_a_review_view_takes_only_its_ticks() {
    let (mut app, fx, t0) = largest();
    // In Files: mark `small`, the second row.
    app.key(press(KeyCode::Down), t0);
    app.key(press(KeyCode::Char(' ')), t0);
    assert_eq!(app.marks.items().len(), 1);
    views::switch(&mut app, View::Largest);
    sync(&mut app, &fx.m, t0);
    render(&mut app, t0, 100, 20);
    // `small` is listed too: Largest lists every file below. Tick `a.iso` only.
    app.key(press(KeyCode::Home), t0);
    app.key(press(KeyCode::Char(' ')), t0);
    assert_eq!(app.marks.items().len(), 2);
    let fx_out = keys::run(&mut app, Action::Trash, t0);
    assert_eq!(asked(&fx_out), 2, "both are listed in Largest");
    app.overlay = Overlay::None;
    // A mark outside what the view lists stays out of its delete.
    let (mut app, fx, t0) = largest();
    app.key(press(KeyCode::Char(' ')), t0);
    views::switch(&mut app, View::Largest);
    sync(&mut app, &fx.m, t0);
    render(&mut app, t0, 100, 20);
    app.key(press(KeyCode::Char(' ')), t0);
    assert_eq!(app.marks.items().len(), 2, "big/ and a.iso");
    let fx_out = app.key(press(KeyCode::Char('d')), t0);
    assert_eq!(asked(&fx_out), 1, "a.iso only, not big/");
}
