use std::fmt::Write as _;
use std::sync::Arc;

use dfdu_store::history::Growth;
use dfdu_tree::Totals;
use dfdu_tree::filetype::TypeFilter;
use ratatui::buffer::Buffer;
use ratatui::crossterm::event::KeyCode;

use super::*;
use crate::model::snapshot::Snapshot;
use crate::tests::{press, render_buffer, snap_without_totals, style_at, text};

fn texts(dir: &str, root: &str, home: Option<&str>) -> Vec<String> {
    segments(Path::new(dir), Path::new(root), home.map(Path::new))
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
    let first = first_line(&buf);
    assert!(
        first.starts_with(" dfdu  /srv/data › dev › rust "),
        "{first}"
    );
    // The current directory is styled as the title, the way to it dimmed.
    assert_eq!(style_at(&buf, "rust"), style_at(&buf, "dfdu"));
    assert_eq!(style_at(&buf, "dev"), style_at(&buf, "›"));
    assert_ne!(style_at(&buf, "dev"), style_at(&buf, "rust"));
    let children: Vec<_> = app.mouse.crumbs.iter().map(|c| c.child.clone()).collect();
    assert_eq!(
        children,
        ["/srv/data/dev", "/srv/data/dev/rust"].map(PathBuf::from)
    );
}

#[test]
fn long_paths_collapse_from_the_left() {
    let dir = "/srv/data/projects/rust/dfdu/src/ui";
    let (app, buf) = browser(dir, "/srv/data", Theme::no_color(), 40);
    let header: String = text(&buf).lines().take(2).flat_map(|l| [l, "\n"]).collect();
    insta::assert_snapshot!("breadcrumbs_collapsed", header);
    assert!(header.starts_with(" dfdu  … › src › ui "), "{header}");
    let crumbs: Vec<_> = app
        .mouse
        .crumbs
        .iter()
        .map(|c| (c.area, c.child.clone()))
        .collect();
    assert_eq!(crumbs, [(Rect::new(11, 0, 3, 1), PathBuf::from(dir))]);

    let (_, buf) = browser(dir, "/srv/data", Theme::no_color(), 100);
    let first = first_line(&buf);
    assert!(
        first.starts_with(" dfdu  /srv/data › projects › rust › dfdu › src › ui "),
        "{first}"
    );
}

/// The first line of a read-only browser at the scan root `root`, done scanning, drawn
/// `width` columns wide.
fn done_at_root(root: &str, width: u16) -> String {
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
    first_line(&render_buffer(&mut app, t0, width, 8))
}

#[test]
fn the_scan_state_shortens_before_the_path_does() {
    let root = "/Volumes/Backup Drive/archive/client-deliverables";
    let at = |width| done_at_root(root, width);
    assert!(
        at(100).ends_with(" read-only · done in 00:00"),
        "{}",
        at(100)
    );
    assert!(at(80).ends_with(" read-only · done 00:00"), "{}", at(80));
    let first = at(70);
    assert!(
        first.contains(root) && first.ends_with(" done 00:00"),
        "{first}"
    );
    assert!(!first.contains("read-only"), "{first}");
    // Then the scan root itself gives way, in the middle.
    let first = at(50);
    assert!(!first.contains(root) && first.contains('…'), "{first}");
    assert!(first.ends_with("deliverables 00:00"), "{first}");
    // The last cell, a space, is trimmed.
    assert_eq!(Span::raw(&first).width(), 49, "{first}");
}

#[test]
fn a_long_scan_root_is_shortened_in_the_middle() {
    let root = "/Volumes/Backup Drive/archive/projects/2026/client-deliverables/final";
    insta::assert_snapshot!("long_root_70", done_at_root(root, 70));
    insta::assert_snapshot!("long_root_50", done_at_root(root, 50));
    // Below the root, the current directory keeps its name and the root collapses first.
    let (_, buf) = browser(&format!("{root}/renders"), root, Theme::no_color(), 50);
    let first = first_line(&buf);
    assert!(first.starts_with(" dfdu  … › renders "), "{first}");
    assert!(first.contains("scanning 00:00"), "{first}");
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
    let first = first_line(&render_buffer(&mut app, t0, 80, 8));
    assert!(first.starts_with(" dfdu  /home/jo › dev "), "{first}");

    app.home = Some("/home/jo".into());
    let first = first_line(&render_buffer(&mut app, t0, 80, 8));
    assert!(first.starts_with(" dfdu  ~ › dev "), "{first}");
    let crumbs: Vec<_> = app.mouse.crumbs.iter().map(|c| c.child.clone()).collect();
    assert_eq!(crumbs, [PathBuf::from("/home/jo/dev")]);
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
    assert!(
        at(120).is_some_and(|l| l.contains("Δ since 2026-10-18")),
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
