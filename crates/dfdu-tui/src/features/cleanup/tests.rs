use std::time::Instant;

use dfdu_engine::{Command, Model};
use dfdu_tree::{Class, HostPath, Kind, Safety, now_secs};
use ratatui::crossterm::event::KeyCode;

use super::*;
use crate::state::marks;
use crate::tests::{classified, classified_app, sync};
use crate::tests::{press, render};
use crate::theme::Theme;

mod native;

/// A browser of `m` read at `t0`, then with the view opened and read again, as the run loop
/// does.
pub(crate) fn opened(m: &Model, t0: Instant) -> App {
    let mut app = App::new(Theme::no_color(), t0);
    sync(&mut app, m, t0);
    app.key(press(KeyCode::Char('3')), t0);
    sync(&mut app, m, t0);
    app
}

#[test]
fn groups_by_category_and_safety_and_marks_the_recommended() {
    let t0 = Instant::now();
    let (fx, mut app) = classified_app(t0);
    let fx_out = app.key(press(KeyCode::Char('3')), t0);
    assert!(
        matches!(fx_out.as_slice(), [Effect::Send(Command::NativeSizes)]),
        "{fx_out:?}"
    );
    sync(&mut app, &fx.m, t0);
    assert!(active(&app));
    let items = &app.session.extras.cleanup.items;
    assert_eq!(items.len(), 2, "{items:?}");
    // Both are below their rules' minimum sizes: nothing is recommended.
    assert!(app.session.marks.is_empty());
    let frame = render(&mut app, t0, 100, 24);
    assert!(
        frame.contains("Selected 0 B of 70.3 KiB · 0 of 2 items"),
        "{frame}"
    );
    assert!(frame.contains("▾ [ ] Safe to delete · "), "{frame}");
    assert!(frame.contains("▸ [ ] OS and user caches"), "{frame}");
    insta::assert_snapshot!("cleanup_100x24", frame);

    // The first section is the safe one, and its first category below it; Enter opens it,
    // Space ticks its item and again unticks it.
    app.key(press(KeyCode::Down), t0);
    app.key(press(KeyCode::Enter), t0);
    let frame = render(&mut app, t0, 100, 24);
    assert!(frame.contains("▾ [ ] OS and user caches"), "{frame}");
    app.key(press(KeyCode::Down), t0);
    app.key(press(KeyCode::Char(' ')), t0);
    assert!(
        app.session.marks.items()[0]
            .target
            .path
            .to_local()
            .ends_with("mid/m0")
    );
    app.key(press(KeyCode::Up), t0);
    app.key(press(KeyCode::Char(' ')), t0);
    assert!(
        app.session.marks.is_empty(),
        "Space on a group toggles all of it"
    );
    // On the section, Space ticks the whole level, and `←` closes it.
    app.key(press(KeyCode::Up), t0);
    app.key(press(KeyCode::Char(' ')), t0);
    assert_eq!(app.session.marks.items().len(), 1);
    let frame = render(&mut app, t0, 100, 24);
    assert!(frame.contains("▾ [x] Safe to delete"), "{frame}");
    app.key(press(KeyCode::Char('X')), t0);
    assert!(app.session.marks.is_empty(), "X unticks everything");
    app.key(press(KeyCode::Home), t0);
    app.key(press(KeyCode::Left), t0);
    let frame = render(&mut app, t0, 100, 24);
    assert!(frame.contains("▸ [ ] Safe to delete"), "{frame}");
    assert!(!frame.contains("OS and user caches"), "{frame}");
    app.key(press(KeyCode::Right), t0);
    // Backspace closes it too, as in the list, and leaves the list behind where it was.
    let dir = app.session.snap.dir_path.clone();
    app.key(press(KeyCode::Backspace), t0);
    let frame = render(&mut app, t0, 100, 24);
    assert!(frame.contains("▸ [ ] Safe to delete"), "{frame}");
    assert_eq!(app.session.snap.dir_path, dir);
    app.key(press(KeyCode::Right), t0);
    // `f` shows safe items only: the regenerable group goes.
    app.key(press(KeyCode::Char('f')), t0);
    let frame = render(&mut app, t0, 100, 24);
    assert!(!frame.contains("Build output"), "{frame}");
    // Back to the list.
    app.key(press(KeyCode::Char('1')), t0);
    assert!(!active(&app));
}

/// [`classified`] with `/r/old-cache`, a file of 1 GiB unchanged for years under a safe rule:
/// the one recommended item.
fn with_recommended() -> crate::tests::Fixture {
    let mut fx = classified();
    let t = &mut fx.m.tree;
    let root = t.roots().next().unwrap();
    let meta = crate::tests::meta(Kind::File, 1 << 30, 900);
    let f = t.add_file(root, b"old-cache", &meta, false, None);
    let own = t.own(f);
    t.propagate_add(root, &own);
    let rules = fx.m.rules.as_ref().unwrap();
    let class = Class {
        rule: rules.index("generic.cachedir-tag").unwrap(),
        safety: Safety::Safe,
        degraded: None,
        flags: dfdu_tree::ClassFlags::NONE,
    };
    fx.m.tree.set_class(f, Some(class));
    fx
}

#[test]
fn recommended_items_are_marked_only_when_the_view_opens() {
    let fx = with_recommended();
    let t0 = Instant::now();
    let mut app = App::new(Theme::no_color(), t0);
    sync(&mut app, &fx.m, t0);
    // On without the user opening the view: the read takes the matches in, and marks nothing.
    app.session.view = crate::app::views::View::Cleanup;
    sync(&mut app, &fx.m, t0);
    assert_eq!(app.session.extras.cleanup.items.len(), 3);
    assert!(app.session.marks.is_empty());
    app.key(press(KeyCode::Char('1')), t0);

    // Opening it marks the recommended item, and says so.
    app.key(press(KeyCode::Char('3')), t0);
    sync(&mut app, &fx.m, t0);
    let marked: Vec<_> = app
        .session
        .marks
        .items()
        .iter()
        .map(|i| &i.target.path)
        .collect();
    assert_eq!(marked, [&HostPath::from("/r").join(b"old-cache")]);
    assert_eq!(
        app.session.status.as_deref(),
        Some("Ticked 1 suggested item (1.0 GiB) · X untick all")
    );
    // Said until the next key.
    app.key(press(KeyCode::Down), t0);
    assert_eq!(app.session.status, None);
    // Unmarked, it stays so: through later reads, and when the view opens again.
    marks::clear(&mut app);
    sync(&mut app, &fx.m, t0);
    app.key(press(KeyCode::Char('1')), t0);
    app.key(press(KeyCode::Char('3')), t0);
    sync(&mut app, &fx.m, t0);
    assert!(app.session.marks.is_empty());
}

/// The group rows' columns are as wide as the categories shown, a category found at two safety
/// levels shows each level, and a long path is shortened in the middle.
#[test]
fn group_columns_fit_the_categories_shown() {
    let fx = classified();
    let t0 = Instant::now();
    let mut app = opened(&fx.m, t0);
    let rules = app.session.snap.rules.clone().unwrap();
    let base = app.session.extras.cleanup.items[0].clone();
    // Three and a half days old, so the age shown stays the same during the test.
    let mtime = now_secs() - 3 * 86_400 - 43_200;
    let it = |id: &str, safety: Safety, path: &str| Item {
        class: Class {
            rule: rules.index(id).unwrap(),
            safety,
            ..base.class
        },
        path: path.into(),
        mtime,
        ..base.clone()
    };
    let long = "/r/clients/northwind-traders/website-redesign-2026/frontend/node_modules";
    app.session.extras.cleanup.items = vec![
        it(
            "node.node_modules",
            Safety::Regenerable,
            "/r/web/node_modules",
        ),
        it("node.node_modules", Safety::Review, long),
        it("rust.target", Safety::Regenerable, "/r/target"),
    ];
    for row in rows(&app) {
        if let Row::Group { cat, safety } = row {
            app.session.cleanup.open.insert((cat, safety));
        }
    }
    let frame = render(&mut app, t0, 80, 20);
    insta::assert_snapshot!("cleanup_columns_80x20", frame);
    let deps: Vec<&str> = (frame.lines())
        .filter(|l| l.contains("Project dependencies"))
        .collect();
    assert_eq!(deps.len(), 2, "{frame}");
    // Each under the section of its level.
    let line_of = |s: &str| frame.lines().position(|l| l.contains(s)).unwrap();
    let (regenerable, review) = (line_of("Regenerable · "), line_of("Review first · "));
    let first_deps = line_of("Project dependencies");
    assert!(regenerable < first_deps && first_deps < review, "{frame}");
    let at = |l: &str| l.find("KiB").unwrap();
    assert_eq!(at(deps[0]), at(deps[1]), "{frame}");
    assert!(
        !frame.contains(long) && frame.contains("node_modules"),
        "{frame}"
    );
    assert!(frame.lines().all(|l| l.chars().count() <= 80), "{frame}");
}

/// The details pane describes the item selected in the cleanup view, not the list's entry.
#[test]
fn details_follow_the_cleanup_selection() {
    let fx = classified();
    let t0 = Instant::now();
    let mut app = opened(&fx.m, t0);
    app.key(press(KeyCode::Down), t0);
    app.key(press(KeyCode::Enter), t0);
    app.key(press(KeyCode::Down), t0);
    app.key(press(KeyCode::Char('i')), t0);
    sync(&mut app, &fx.m, t0);
    let want = selected(&app).unwrap().path.clone();
    let facts = app
        .session
        .extras
        .details
        .facts
        .as_ref()
        .expect("details read");
    assert_eq!(facts.path, want);
    // On a group row there is nothing to describe.
    app.key(press(KeyCode::Up), t0);
    sync(&mut app, &fx.m, t0);
    assert!(app.session.extras.details.facts.is_none());
}

/// With rules but nothing they match, the view says what it looks for and how to go back.
#[test]
fn no_matches_have_an_empty_state() {
    let mut fx = crate::tests::tree();
    let env = dfdu_rules::Env::with(dfdu_rules::Platform::Linux, &[("HOME", "/home/u")]);
    fx.m.rules = Some(std::sync::Arc::new(dfdu_rules::builtin(&env).0));
    let t0 = Instant::now();
    let mut app = opened(&fx.m, t0);
    assert!(active(&app) && app.session.extras.cleanup.items.is_empty());
    let frame = render(&mut app, t0, 100, 20);
    assert!(frame.contains("╭ Cleanup ─"), "{frame}");
    assert!(frame.contains("Esc  back to Files"), "{frame}");
    assert!(
        frame.contains("Nothing matched yet") || frame.contains("Nothing the rules know"),
        "{frame}"
    );
}

/// On a short terminal the rule text gives way so that the list still shows rows.
#[test]
fn a_short_terminal_still_shows_the_groups() {
    let fx = classified();
    let t0 = Instant::now();
    let mut app = opened(&fx.m, t0);
    let frame = render(&mut app, t0, 100, 10);
    assert!(frame.contains("▸ [ ] OS and user caches"), "{frame}");
}

/// Each item row says in a word why it is or is not suggested, and the detail pane explains the
/// selected item's rule and the reason in full.
#[test]
fn rows_say_why_and_the_rule_is_explained_below() {
    let fx = with_recommended();
    let t0 = Instant::now();
    let mut app = opened(&fx.m, t0);
    for row in rows(&app) {
        if let Row::Group { cat, safety } = row {
            app.session.cleanup.open.insert((cat, safety));
        }
    }
    let frame = render(&mut app, t0, 100, 24);
    assert!(!frame.contains("Enter opens a group"), "{frame}");
    let row = |frame: &str, path: &str| {
        frame
            .lines()
            .find(|l| l.contains(path))
            .unwrap_or_else(|| panic!("{path}: {frame}"))
            .trim_end()
            .to_owned()
    };
    assert!(
        row(&frame, "/r/old-cache").ends_with("✓ suggested"),
        "{frame}"
    );
    // `m0` and `big` are below their rules' minimum sizes.
    assert!(row(&frame, "/r/mid/m0").ends_with("small"), "{frame}");
    assert!(row(&frame, "/r/big").ends_with("small"), "{frame}");
    insta::assert_snapshot!("cleanup_notes_100x24", frame);

    // On an item, its rule and the reason in full in the pane below the rows.
    let items = app.session.extras.cleanup.items.clone();
    let at = items
        .iter()
        .position(|it| it.path.to_local().ends_with("old-cache"))
        .unwrap();
    app.session.cleanup.cursor = rows(&app).iter().position(|r| *r == Row::Item(at)).unwrap();
    let frame = render(&mut app, t0, 100, 24);
    let rules = app.session.snap.rules.clone().unwrap();
    let name = &rules.rule(items[at].class.rule).name;
    let lines: Vec<&str> = frame.lines().collect();
    let last = lines.iter().rposition(|l| l.contains("/r/")).unwrap();
    let rule = lines
        .iter()
        .position(|l| l.contains(name.as_str()))
        .unwrap();
    assert!(rule > last, "{frame}");
    assert!(frame.contains("Suggested: safe to delete."), "{frame}");
    assert!(frame.contains(" You lose: "), "{frame}");
    // From 110 columns the pane is on the right of the rows.
    let wide = render(&mut app, t0, 120, 24);
    let line = (wide.lines())
        .find(|l| l.contains("/r/old-cache"))
        .unwrap_or_default();
    assert!(line.contains(" │ "), "{wide}");
}

#[test]
fn half_pages_move_the_cursor() {
    let fx = classified();
    let t0 = Instant::now();
    let mut app = opened(&fx.m, t0);
    let ctrl = |c| {
        KeyEvent::new(
            KeyCode::Char(c),
            ratatui::crossterm::event::KeyModifiers::CONTROL,
        )
    };
    let last = rows(&app).len() - 1;
    app.key(ctrl('d'), t0);
    assert_eq!(app.session.cleanup.cursor, last.min(5));
    app.key(ctrl('u'), t0);
    assert_eq!(app.session.cleanup.cursor, 0);
}
