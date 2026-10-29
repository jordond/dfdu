use std::time::Instant;

use ratatui::crossterm::event::KeyCode;

use super::*;
use crate::features::treemap::tests::{classified, sync};
use crate::tests::{press, render};
use crate::theme::Theme;

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
    let out = app.key(press(KeyCode::Char('n')), t0);
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

/// The user caches yield to the caches inside them: listed for the rest, and marking them
/// marks that rest, not the folder whole.
#[test]
fn marking_a_container_marks_its_rest() {
    let fx = crate::tests::caches::caches();
    let cache = fx.find("Caches");
    let m = Model {
        tree: fx.tree,
        rules: Some(std::sync::Arc::new(fx.rules)),
        ..Model::default()
    };
    let t0 = Instant::now();
    let mut app = App::new(Theme::no_color(), t0);
    sync(&mut app, &m, t0);
    app.key(press(KeyCode::Char('3')), t0);
    sync(&mut app, &m, t0);
    let items = app.extras.cleanup.items.clone();
    let at = items
        .iter()
        .position(|it| it.path.ends_with("Caches"))
        .expect("the user caches are listed");
    let it = &items[at];
    assert_eq!(it.size, m.tree.total(cache).alloc - 500);
    let mut rest: Vec<&std::path::Path> = it.rest.iter().map(|r| &*r.path).collect();
    rest.sort();
    assert_eq!(rest.len(), 2, "{rest:?}");
    assert!(items.iter().any(|i| i.path.ends_with("Caches/pip")));
    let cat = app
        .snap
        .rules
        .as_ref()
        .unwrap()
        .rule(it.class.rule)
        .category;
    app.cleanup.open.insert((cat, it.class.safety));
    app.cleanup.cursor = rows(&app).iter().position(|r| *r == Row::Item(at)).unwrap();
    app.key(press(KeyCode::Char(' ')), t0);
    let mut marked: Vec<&std::path::Path> =
        app.marks.items().iter().map(|i| &*i.target.path).collect();
    marked.sort();
    assert_eq!(marked, rest);
    assert!(is_marked(&app, it));
    app.cleanup.cursor -= 1;
    app.key(press(KeyCode::Char(' ')), t0);
    assert!(app.marks.is_empty());
}
