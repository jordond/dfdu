use std::time::Instant;

use ratatui::crossterm::event::KeyCode;

use super::*;
use crate::tests::{press, render};
use crate::theme::Theme;
use crate::treemap::tests::{classified, sync};

#[test]
fn groups_by_category_and_safety_and_marks_the_recommended() {
    let fx = classified();
    let t0 = Instant::now();
    let mut app = App::new(Theme::no_color(), t0);
    sync(&mut app, &fx.m, t0);
    let fx_out = app.key(press(KeyCode::Char('3')), t0);
    assert!(
        matches!(fx_out.as_slice(), [Effect::Send(Command::NativeSizes)]),
        "{fx_out:?}"
    );
    sync(&mut app, &fx.m, t0);
    assert!(active(&app));
    let items = &app.extras.cleanup.items;
    assert_eq!(items.len(), 2, "{items:?}");
    // Both are below their rules' minimum sizes: nothing is recommended.
    assert!(app.marks.is_empty());
    let frame = render(&mut app, t0, 100, 24);
    assert!(frame.contains("reclaimable"), "{frame}");
    assert!(frame.contains("▸ OS and user caches"), "{frame}");
    insta::assert_snapshot!("cleanup_100x24", frame);

    // The first group is the safe one; Enter opens it, Space marks its item and again
    // unmarks it.
    app.key(press(KeyCode::Enter), t0);
    let frame = render(&mut app, t0, 100, 24);
    assert!(frame.contains("▾ OS and user caches"), "{frame}");
    app.key(press(KeyCode::Down), t0);
    app.key(press(KeyCode::Char(' ')), t0);
    assert!(app.marks.items()[0].target.path.ends_with("mid/m0"));
    app.key(press(KeyCode::Up), t0);
    app.key(press(KeyCode::Char(' ')), t0);
    assert!(app.marks.is_empty(), "Space on a group toggles all of it");
    // `f` shows safe items only: the regenerable group goes.
    app.key(press(KeyCode::Char('f')), t0);
    let frame = render(&mut app, t0, 100, 24);
    assert!(!frame.contains("Build output"), "{frame}");
    // Back to the list.
    app.key(press(KeyCode::Char('1')), t0);
    assert!(!active(&app));
}

/// The details pane describes the item selected in the cleanup view, not the list's entry.
#[test]
fn details_follow_the_cleanup_selection() {
    let fx = classified();
    let t0 = Instant::now();
    let mut app = App::new(Theme::no_color(), t0);
    sync(&mut app, &fx.m, t0);
    app.key(press(KeyCode::Char('3')), t0);
    sync(&mut app, &fx.m, t0);
    app.key(press(KeyCode::Enter), t0);
    app.key(press(KeyCode::Down), t0);
    app.key(press(KeyCode::Char('i')), t0);
    sync(&mut app, &fx.m, t0);
    let want = selected_item(&app).unwrap().path.clone();
    let facts = app.extras.details.facts.as_ref().expect("details read");
    assert_eq!(facts.path, want);
    // On a group row there is nothing to describe.
    app.key(press(KeyCode::Up), t0);
    sync(&mut app, &fx.m, t0);
    assert!(app.extras.details.facts.is_none());
}

/// On a short terminal the rule text gives way so that the list still shows rows.
#[test]
fn a_short_terminal_still_shows_the_groups() {
    let fx = classified();
    let t0 = Instant::now();
    let mut app = App::new(Theme::no_color(), t0);
    sync(&mut app, &fx.m, t0);
    app.key(press(KeyCode::Char('3')), t0);
    sync(&mut app, &fx.m, t0);
    let frame = render(&mut app, t0, 100, 10);
    assert!(frame.contains("▸ OS and user caches"), "{frame}");
}

#[test]
fn recommended_items() {
    let fx = classified();
    let t0 = Instant::now();
    let mut app = App::new(Theme::no_color(), t0);
    sync(&mut app, &fx.m, t0);
    app.key(press(KeyCode::Char('3')), t0);
    sync(&mut app, &fx.m, t0);
    let rules = app.snap.rules.clone().unwrap();
    let base = app.extras.cleanup.items[0].clone();
    let rule = |id: &str| rules.index(id).unwrap();
    let now = now_secs();
    let day = 86_400;
    let it = |id: &str, safety: Safety, size: u64, age_days: i64, flags: u8| Item {
        class: Class {
            rule: rule(id),
            safety,
            flags,
            ..base.class
        },
        size,
        mtime: now - age_days * day,
        ..base.clone()
    };
    let gib = 1 << 30;
    // Safe above its minimum (50 MiB), quiet for a week.
    assert!(recommended(
        &app,
        &it("generic.cachedir-tag", Safety::Safe, gib, 8, 0),
        now
    ));
    assert!(!recommended(
        &app,
        &it("generic.cachedir-tag", Safety::Safe, 1 << 20, 8, 0),
        now
    ));
    assert!(!recommended(
        &app,
        &it("generic.cachedir-tag", Safety::Safe, gib, 2, 0),
        now
    ));
    // Regenerable only when stale (the Rust rule: 14 days).
    assert!(recommended(
        &app,
        &it("rust.target", Safety::Regenerable, gib, 15, 0),
        now
    ));
    assert!(!recommended(
        &app,
        &it("rust.target", Safety::Regenerable, gib, 10, 0),
        now
    ));
    // Never review, nothing in use or tracked by git.
    assert!(!recommended(
        &app,
        &it("rust.target", Safety::Review, gib, 99, 0),
        now
    ));
    let in_use = Class::IN_USE;
    assert!(!recommended(
        &app,
        &it("rust.target", Safety::Regenerable, gib, 99, in_use),
        now
    ));
}

#[test]
fn native_commands_start_with_their_dry_run() {
    let fx = classified();
    let t0 = Instant::now();
    let mut app = App::new(Theme::no_color(), t0);
    sync(&mut app, &fx.m, t0);
    app.key(press(KeyCode::Char('3')), t0);
    sync(&mut app, &fx.m, t0);
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
    let out = app.key(press(KeyCode::Char('n')), t0);
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
