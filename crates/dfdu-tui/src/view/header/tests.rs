use std::fmt::Write as _;
use std::sync::Arc;

use dfdu_store::history::Growth;
use dfdu_tree::Totals;
use dfdu_tree::filetype::TypeFilter;
use ratatui::buffer::Buffer;
use ratatui::crossterm::event::KeyCode;
use ratatui::style::Modifier;

use super::*;
use crate::model::snapshot::Snapshot;
use crate::tests::{below, press, render_buffer, snap_without_totals, style_at, text};

fn texts(dir: &str, root: &str, home: Option<&str>) -> Vec<String> {
    segments(&dir.into(), &root.into(), home.map(Path::new))
        .into_iter()
        .map(|s| s.text)
        .collect()
}

/// A browser of `dir` below the scan root `root`, drawn `width` columns wide.
fn browser(dir: &str, root: &str, theme: Theme, width: u16) -> (App, Buffer) {
    let t0 = Instant::now();
    let mut app = App::new(theme, t0);
    let below = Snapshot {
        dir_path: dir.into(),
        root_path: root.into(),
        at_root: dir == root,
        ..snap_without_totals(Vec::new(), 1)
    };
    app.refresh(below, t0);
    let buf = render_buffer(&mut app, t0, width, 8);
    (app, buf)
}

fn first_line(buf: &Buffer) -> String {
    text(buf).lines().next().unwrap_or_default().to_owned()
}

/// The scope line: the second line of the header.
fn scope_line(buf: &Buffer) -> String {
    text(buf).lines().nth(1).unwrap_or_default().to_owned()
}

#[test]
fn breadcrumbs_show_path_below_root() {
    assert_eq!(
        texts("/srv/data/dev/rust", "/srv/data", None),
        ["/srv/data", "dev", "rust"]
    );
    assert_eq!(texts("/srv/data", "/srv/data", None), ["/srv/data"]);
    // Before the first snapshot nothing is known about the root.
    assert_eq!(texts("/srv/data/dev", "", None), ["/srv/data/dev"]);

    let (app, buf) = browser("/srv/data/dev/rust", "/srv/data", Theme::ansi16(), 100);
    let scope = scope_line(&buf);
    assert!(scope.starts_with(" /srv/data › dev › rust "), "{scope}");
    // The current directory is styled as the title, the way to it dimmed.
    assert_eq!(style_at(&buf, "rust"), style_at(&buf, "dfdu"));
    assert_eq!(style_at(&buf, "dev"), style_at(&buf, "›"));
    assert_ne!(style_at(&buf, "dev"), style_at(&buf, "rust"));
    let children: Vec<_> = app.mouse.crumbs.iter().map(|c| c.child.clone()).collect();
    assert_eq!(
        children,
        [below("/srv/data", "dev"), below("/srv/data", "dev/rust")]
    );
}

#[test]
fn long_paths_collapse_from_the_left() {
    let dir = "/srv/data/projects/rust/dfdu/src/ui";
    let (app, buf) = browser(dir, "/srv/data", Theme::no_color(), 48);
    let header: String = text(&buf).lines().take(2).flat_map(|l| [l, "\n"]).collect();
    insta::assert_snapshot!("breadcrumbs_collapsed", header);
    let scope = scope_line(&buf);
    assert!(scope.starts_with(" … › src › ui "), "{scope}");
    let crumbs: Vec<_> = app
        .mouse
        .crumbs
        .iter()
        .map(|c| (c.area, c.child.clone()))
        .collect();
    assert_eq!(
        crumbs,
        [(
            Rect::new(5, 1, 3, 1),
            below("/srv/data", "projects/rust/dfdu/src/ui")
        )]
    );

    let (_, buf) = browser(dir, "/srv/data", Theme::no_color(), 100);
    let scope = scope_line(&buf);
    assert!(
        scope.starts_with(" /srv/data › projects › rust › dfdu › src › ui "),
        "{scope}"
    );
}

/// The header of a read-only browser at the scan root `root`, done scanning, drawn `width`
/// columns wide: the tab bar and the scope line.
fn done_at_root(root: &str, width: u16) -> (String, String) {
    let t0 = Instant::now();
    let mut app = App::new(Theme::no_color(), t0);
    app.read_only = true;
    let snap = Snapshot {
        dir_path: root.into(),
        root_path: root.into(),
        busy: false,
        ..snap_without_totals(Vec::new(), 1)
    };
    app.refresh(snap, t0);
    let buf = render_buffer(&mut app, t0, width, 8);
    (first_line(&buf), scope_line(&buf))
}

#[test]
fn the_scan_state_shortens_before_the_tabs_do() {
    let root = "/srv/archive";
    let at = |width| done_at_root(root, width).0;
    let wide = at(100);
    assert!(wide.contains("│ 5 Largest "), "{wide}");
    assert!(wide.ends_with(" read-only · done in 00:00"), "{wide}");
    // Then the state gives way to the named tabs...
    let first = at(80);
    assert!(first.contains("│ 5 Largest"), "{first}");
    assert!(first.ends_with(" done 00:00"), "{first}");
    assert!(!first.contains("read-only"), "{first}");
    // ...and below 80 columns only the view shown is named.
    let first = at(79);
    assert!(first.contains("│ [1 Files] 2 3 4 5 "), "{first}");
    assert!(first.ends_with(" read-only · done in 00:00"), "{first}");
    let first = at(32);
    assert!(first.ends_with(" 00:00"), "{first}");
    // The last cell, a space, is trimmed.
    assert_eq!(Span::raw(&first).width(), 31, "{first}");
}

#[test]
fn a_long_scan_root_is_shortened_in_the_middle() {
    let root = "/Volumes/Backup Drive/archive/projects/2026/client-deliverables/final";
    insta::assert_snapshot!("long_root_70", done_at_root(root, 70).1);
    insta::assert_snapshot!("long_root_50", done_at_root(root, 50).1);
    // Below the root, the current directory keeps its name and the root collapses first.
    let (_, buf) = browser(&format!("{root}/renders"), root, Theme::no_color(), 50);
    let scope = scope_line(&buf);
    assert!(scope.starts_with(" … › renders "), "{scope}");
    assert!(first_line(&buf).contains("scanning 00:00"), "{buf:?}");
}

#[test]
fn home_is_shown_as_tilde() {
    let home = Some("/home/jo");
    assert_eq!(
        texts("/home/jo/dev/rust", "/home/jo", home),
        ["~", "dev", "rust"]
    );
    assert_eq!(
        texts("/home/jo/dev/rust", "/home/jo/dev", home),
        // Joined as paths are, with `\` on Windows.
        [
            Path::new("~").join("dev").display().to_string(),
            "rust".into()
        ]
    );
    assert_eq!(
        texts("/home/joe/dev", "/home/joe", home),
        ["/home/joe", "dev"]
    );
    assert_eq!(texts("/srv", "/srv", home), ["/srv"]);
    // A home directory of `/` is not shown as `~`.
    assert_eq!(texts("/srv/a", "/srv", Some("/")), ["/srv", "a"]);
}

#[test]
fn header_shows_the_home_directory_as_tilde() {
    let t0 = Instant::now();
    let mut app = App::new(Theme::no_color(), t0);
    let below = Snapshot {
        dir_path: "/home/jo/dev".into(),
        root_path: "/home/jo".into(),
        at_root: false,
        ..snap_without_totals(Vec::new(), 1)
    };
    app.refresh(below, t0);
    let scope = scope_line(&render_buffer(&mut app, t0, 80, 8));
    assert!(scope.starts_with(" /home/jo › dev "), "{scope}");

    app.home = Some("/home/jo".into());
    let scope = scope_line(&render_buffer(&mut app, t0, 80, 8));
    assert!(scope.starts_with(" ~ › dev "), "{scope}");
    let crumbs: Vec<_> = app.mouse.crumbs.iter().map(|c| c.child.clone()).collect();
    assert_eq!(crumbs, [crate::tests::below("/home/jo", "dev")]);
}

/// 2026-10-18 12:00 UTC, when the last recorded scan finished.
const BASELINE: i64 = 1_792_324_800;

/// The widths of the mockups, and a narrow one.
const WIDTHS: [u16; 4] = [120, 80, 64, 48];

/// A browser at `/home/jo/dev`, done scanning 4.5M items of 163.3 GiB, set up by `setup`.
fn dev(setup: &dyn Fn(&mut App, Instant), types: &str) -> (App, Instant) {
    let t0 = Instant::now();
    let mut app = App::new(Theme::no_color(), t0);
    let size = 163 * (1 << 30) + 3 * (1 << 30) / 10;
    let total = Totals {
        apparent: size,
        alloc: size,
        referenced: size,
        items: 4_500_000,
        ..Totals::default()
    };
    let snap = Snapshot {
        dir_path: "/home/jo/dev".into(),
        root_path: "/home/jo/dev".into(),
        busy: false,
        dir_total: total,
        root_total: total,
        types: TypeFilter::parse(types).expect("types"),
        ..snap_without_totals(Vec::new(), 1)
    };
    app.refresh(snap, t0);
    setup(&mut app, t0);
    app.reorder(t0);
    (app, t0)
}

/// The second line of [`dev`] at `width` columns.
fn second_line(app: &mut App, t0: Instant, width: u16) -> String {
    let buf = render_buffer(app, t0, width, 8);
    text(&buf).lines().nth(1).unwrap_or_default().to_owned()
}

/// The second line of [`dev`] at each of [`WIDTHS`], one per line after the width.
fn modes(setup: &dyn Fn(&mut App, Instant), types: &str) -> String {
    let (mut app, t0) = dev(setup, types);
    let mut out = String::new();
    for width in WIDTHS {
        let line = second_line(&mut app, t0, width);
        writeln!(out, "{width:>3}|{line}").expect("write");
    }
    out
}

fn history(app: &mut App, _: Instant) {
    app.growth = Some(Arc::new(Growth {
        root: "/home/jo/dev".into(),
        at: BASELINE,
        sizes: std::collections::BTreeMap::new(),
    }));
}

fn hidden(app: &mut App, _: Instant) {
    app.hide_hidden = true;
}

fn apparent(app: &mut App, _: Instant) {
    app.size_mode = SizeMode::Apparent;
}

fn du_like(app: &mut App, _: Instant) {
    app.size_mode = SizeMode::Referenced;
}

/// Types `/kmp` and keeps it with `Enter`.
fn kept_filter(app: &mut App, t0: Instant) {
    for c in "/kmp".chars() {
        app.key(press(KeyCode::Char(c)), t0);
    }
    app.key(press(KeyCode::Enter), t0);
}

#[test]
fn the_sort_and_the_size_shown_step_with_the_width() {
    insta::assert_snapshot!("modes_default", modes(&|_, _| {}, ""));
}

#[test]
fn each_mode_that_differs_from_the_defaults_is_shown() {
    insta::assert_snapshot!("modes_history", modes(&history, ""));
    insta::assert_snapshot!("modes_hidden", modes(&hidden, ""));
    insta::assert_snapshot!("modes_apparent", modes(&apparent, ""));
    insta::assert_snapshot!("modes_filter", modes(&kept_filter, ""));
    insta::assert_snapshot!("modes_types", modes(&|_, _| {}, "video"));
    // The size `du` counts is not recorded: no changes, so no day of the last scan either.
    let du = modes(
        &|app, t0| {
            history(app, t0);
            du_like(app, t0);
        },
        "",
    );
    insta::assert_snapshot!("modes_du_like", du);
    assert!(!du.contains('Δ'), "{du}");
}

#[test]
fn modes_that_do_not_fit_give_way_from_the_last_with_a_count() {
    let all = |app: &mut App, t0| {
        history(app, t0);
        hidden(app, t0);
        apparent(app, t0);
        kept_filter(app, t0);
    };
    let out = modes(&all, "");
    insta::assert_snapshot!("modes_overflow", out);
    let at = |w: u16| out.lines().find(|l| l.starts_with(&format!("{w:>3}|")));
    // At 120 columns every mode is shown, in its short form beside the scope.
    assert!(
        at(120).is_some_and(|l| l.contains("Δ 10-18") && !l.contains(" · +")),
        "{out}"
    );
    assert!(at(64).is_some_and(|l| l.contains(" · +")), "{out}");
    // A type filter adds to the totals on the left, which leaves less room still.
    insta::assert_snapshot!("modes_overflow_types", modes(&all, "video"));
}

#[test]
fn the_filter_shows_only_once_kept() {
    let (mut app, t0) = dev(
        &|app, t0| {
            for c in "/kmp".chars() {
                app.key(press(KeyCode::Char(c)), t0);
            }
        },
        "",
    );
    let line = second_line(&mut app, t0, 120);
    assert!(!line.contains("/kmp"), "{line}");
    app.key(press(KeyCode::Enter), t0);
    let line = second_line(&mut app, t0, 120);
    assert!(line.contains("/kmp"), "{line}");
}

#[test]
fn a_cell_stays_free_between_the_totals_and_the_modes() {
    let all = |app: &mut App, t0| {
        history(app, t0);
        hidden(app, t0);
        kept_filter(app, t0);
    };
    let (mut app, t0) = dev(&all, "");
    for width in 20..=130 {
        let line = second_line(&mut app, t0, width);
        let rest = line.split_once("163.3 GiB").map_or("", |(_, r)| r);
        assert!(rest.is_empty() || rest.starts_with(' '), "{width}: {line}");
        assert!(
            rest.is_empty() || rest.contains("size ↓"),
            "{width}: {line}"
        );
    }
}

#[test]
fn the_day_of_the_last_scan_matches_the_column_and_the_selection_line() {
    let (mut app, t0) = dev(&history, "");
    let narrow = second_line(&mut app, t0, 80);
    let wide = second_line(&mut app, t0, 120);
    // The column of changes is titled the same way, and the selection line names the day.
    assert!(narrow.ends_with(&growth::column_title(&app)), "{narrow}");
    let day = history::day(BASELINE);
    assert!(wide.ends_with(&format!("Δ since {day}")), "{wide}");
    assert_eq!(
        growth::describe(1 << 30, BASELINE),
        format!("+1.0 GiB since {day}")
    );
}

/// The two header lines of every view at each of [`WIDTHS`], in a directory below the root of
/// the classified tree of the treemap tests (so Cleanup has rules), one block per view.
#[test]
fn every_view_has_its_tab_and_scope_at_each_width() {
    use crate::app::views::{self, View};
    use crate::features::treemap::tests::{classified, sync};
    let fx = classified();
    let t0 = Instant::now();
    let mut app = App::new(Theme::no_color(), t0);
    sync(&mut app, &fx.m, t0);
    let mid = app
        .rows
        .iter()
        .position(|&i| &*app.snap.entries[i].name == b"mid");
    app.select(mid.expect("mid is listed"));
    app.key(press(KeyCode::Enter), t0);
    sync(&mut app, &fx.m, t0);
    let mut out = String::new();
    for view in View::ALL {
        views::switch(&mut app, view);
        sync(&mut app, &fx.m, t0);
        assert_eq!(views::current(&app), view);
        writeln!(out, "{}:", view.name()).expect("write");
        for width in WIDTHS {
            let buf = render_buffer(&mut app, t0, width, 20);
            for line in text(&buf).lines().take(2) {
                writeln!(out, "{width:>3}|{line}").expect("write");
            }
        }
    }
    insta::assert_snapshot!("views_header", out);
    assert!(out.contains("whole scan: /r"), "{out}");
    assert!(out.contains(" /r › mid "), "{out}");
}

#[test]
fn the_active_tab_is_reversed_and_bold_in_every_theme() {
    for theme in [Theme::no_color(), Theme::ansi16()] {
        let (_, buf) = browser("/srv", "/srv", theme.clone(), 120);
        let active = style_at(&buf, " 1 Files ");
        assert!(
            active.add_modifier.contains(Modifier::REVERSED),
            "{active:?}"
        );
        assert!(active.add_modifier.contains(Modifier::BOLD), "{active:?}");
        let other = style_at(&buf, " 2 Map ");
        assert!(
            !other.add_modifier.contains(Modifier::REVERSED),
            "{other:?}"
        );
    }
}

#[test]
fn cleanup_shows_what_it_would_free_once_read() {
    use crate::app::views::{View, badge};
    use crate::features::treemap::tests::{classified, sync};
    let fx = classified();
    let t0 = Instant::now();
    let mut app = App::new(Theme::no_color(), t0);
    sync(&mut app, &fx.m, t0);
    let first = first_line(&render_buffer(&mut app, t0, 120, 8));
    assert!(first.contains("│ 3 Cleanup │"), "nothing read yet: {first}");
    app.key(press(KeyCode::Char('3')), t0);
    sync(&mut app, &fx.m, t0);
    app.key(press(KeyCode::Char('1')), t0);
    let first = first_line(&render_buffer(&mut app, t0, 120, 8));
    let size = badge(&app, View::Cleanup).expect("a badge once read");
    assert!(first.contains(&format!("│ 3 Cleanup {size} │")), "{first}");
}
