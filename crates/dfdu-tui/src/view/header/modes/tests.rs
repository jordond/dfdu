use std::fmt::Write as _;
use std::sync::Arc;
use std::time::Instant;

use dfdu_store::history::Growth;
use dfdu_tree::Totals;
use dfdu_tree::filetype::TypeFilter;
use ratatui::crossterm::event::KeyCode;

use super::*;
use crate::model::snapshot::Snapshot;
use crate::tests::{press, render_buffer, snap_without_totals, text};
use crate::theme::Theme;
use crate::view::header::tests::WIDTHS;

/// 2026-10-18 12:00 UTC, when the last recorded scan finished.
const BASELINE: i64 = 1_792_324_800;

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
