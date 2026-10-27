use std::path::PathBuf;
use std::time::{Duration, Instant};

use ratatui::buffer::Buffer;
use ratatui::crossterm::event::KeyCode;
use ratatui::style::{Color, Modifier};

use super::*;
use crate::snapshot::test_entry;
use crate::tests::{key, render, render_buffer, style_at, styled, text};
use crate::theme::Theme;

const W: u16 = 80;
const H: u16 = 8;

fn ms(n: u64) -> Duration {
    Duration::from_millis(n)
}

fn animated(theme: Theme, t0: Instant) -> App {
    let mut app = App::new(theme, t0);
    start(&mut app);
    app
}

fn dark(level: ColorLevel) -> Theme {
    Theme::named("dark", level, None).expect("dark")
}

/// A snapshot of `/r` while it is scanned, with the entries' sizes as its total.
fn snap(entries: Vec<EntryData>, version: u64) -> Snapshot {
    let mut s = crate::app::tests::snap(entries, version);
    s.dir_total.alloc = s.entries.iter().map(|e| e.size).sum();
    s
}

/// The shown bar fraction of the entry named `name`.
fn shown(app: &App, name: &str) -> f64 {
    let e = app
        .snap
        .entries
        .iter()
        .find(|e| &e.name[..] == name.as_bytes())
        .expect("entry");
    bar(app, e, f64::NAN)
}

fn names(app: &App) -> Vec<String> {
    app.rows
        .iter()
        .map(|&i| String::from_utf8_lossy(&app.snap.entries[i].name).into_owned())
        .collect()
}

/// The list rows of a frame `H` lines high, without the header, status line and footer (they
/// have arrows of their own).
fn list_rows(frame: &str) -> String {
    let rows: Vec<&str> = frame.lines().skip(2).take(H as usize - 4).collect();
    rows.join("\n")
}

/// Draws a frame at each time the motion asks for, up to `until`; returns the time of the last.
fn run_until(app: &mut App, mut now: Instant, until: Instant) -> Instant {
    while let Some(t) = app.motion.next_frame_at().filter(|&t| t <= until) {
        assert!(t > now, "the next frame is not after the last");
        now = t;
        render_buffer(app, now, W, H);
    }
    now
}

#[test]
fn easing_converges_and_stops() {
    let t0 = Instant::now();
    let mut app = animated(Theme::no_color(), t0);
    let mut es = vec![test_entry(0, "a", 1000), test_entry(1, "b", 200)];
    app.refresh(snap(es.clone(), 1), t0);
    render_buffer(&mut app, t0, W, H);
    assert_eq!(shown(&app, "b"), 0.2);
    assert_eq!(app.motion.next_frame_at(), None);

    es[1].size = 800;
    let t1 = t0 + ms(100);
    app.refresh(snap(es, 2), t1);
    render_buffer(&mut app, t1, W, H);
    let mut last = shown(&app, "b");
    assert!(last > 0.2 && last < 0.8, "{last}");
    let mut now = t1;
    while let Some(t) = app.motion.next_frame_at() {
        assert_eq!(t, now + FRAME);
        assert!(t < t1 + Duration::from_secs(1), "still easing after 1 s");
        now = t;
        render_buffer(&mut app, now, W, H);
        let s = shown(&app, "b");
        assert!(s >= last && s <= 0.8, "{last} then {s}");
        last = s;
    }
    assert_eq!(last, 0.8);
    assert_eq!(shown(&app, "a"), 1.0);
}

#[test]
fn entering_a_directory_does_not_animate() {
    let t0 = Instant::now();
    let mut app = animated(Theme::no_color(), t0);
    app.refresh(
        snap(vec![test_entry(0, "a", 1000), test_entry(1, "b", 200)], 1),
        t0,
    );
    render_buffer(&mut app, t0, W, H);
    // The same nodes in another directory, with other sizes: no bar starts from the old length.
    let mut sub = snap(vec![test_entry(0, "a", 100), test_entry(1, "b", 900)], 2);
    sub.dir_path = PathBuf::from("/r/sub");
    sub.dir_node = Some(test_entry(98, "/r/sub", 0).node);
    sub.at_root = false;
    let t1 = t0 + ms(50);
    app.refresh(sub, t1);
    render_buffer(&mut app, t1, W, H);
    assert_eq!(shown(&app, "b"), 1.0);
    assert_eq!(shown(&app, "a"), 100.0 / 900.0);
    assert_eq!(app.motion.next_frame_at(), None);
}

#[test]
fn size_mode_change_and_resize_do_not_animate() {
    let t0 = Instant::now();
    let mut app = animated(Theme::no_color(), t0);
    let mut es = vec![test_entry(0, "a", 1000), test_entry(1, "b", 200)];
    app.refresh(snap(es.clone(), 1), t0);
    render_buffer(&mut app, t0, W, H);

    es[1].size = 500;
    app.size_mode = SizeMode::Apparent;
    let t1 = t0 + ms(50);
    app.refresh(snap(es.clone(), 2), t1);
    render_buffer(&mut app, t1, W, H);
    assert_eq!(shown(&app, "b"), 0.5);
    assert_eq!(app.motion.next_frame_at(), None);

    es[1].size = 900;
    let t2 = t0 + ms(100);
    app.refresh(snap(es, 3), t2);
    render_buffer(&mut app, t2, W + 20, H);
    assert_eq!(shown(&app, "b"), 0.9);
    assert_eq!(app.motion.next_frame_at(), None);
}

/// Entries a, b, c (largest first), drawn; then c grows to the largest.
fn resort_fixture(app: &mut App, t0: Instant) -> Vec<EntryData> {
    let mut es = vec![
        test_entry(0, "a", 300),
        test_entry(1, "b", 200),
        test_entry(2, "c", 100),
    ];
    app.refresh(snap(es.clone(), 1), t0);
    render_buffer(app, t0, W, H);
    es[2].size = 1000;
    es
}

#[test]
fn auto_resort_shows_rank_markers_for_two_seconds() {
    let t0 = Instant::now();
    let mut app = animated(Theme::no_color(), t0);
    let es = resort_fixture(&mut app, t0);
    // After the idle delay and the minimum interval: an automatic re-sort.
    let t1 = t0 + Duration::from_secs(3);
    app.refresh(snap(es, 2), t1);
    assert_eq!(names(&app), ["c", "a", "b"]);
    render_buffer(&mut app, t1, W, H);
    // The bars settle; the markers stay.
    let t = t1 + ms(1500);
    assert!(run_until(&mut app, t1, t) < t1 + Duration::from_secs(1));
    let frame = render(&mut app, t, W, H);
    insta::assert_snapshot!("rank_markers_mono", frame);
    let rows = list_rows(&frame);
    assert!(rows.contains("c  ↑2"), "{frame}");
    assert!(rows.contains("a  ↓1"), "{frame}");
    assert!(rows.contains("b  ↓1"), "{frame}");

    // One frame when they expire, then none.
    assert_eq!(app.motion.next_frame_at(), Some(t1 + MARKERS));
    let frame = render(&mut app, t1 + MARKERS, W, H);
    let rows = list_rows(&frame);
    assert!(!rows.contains('↑') && !rows.contains('↓'), "{frame}");
    assert_eq!(app.motion.next_frame_at(), None);
}

#[test]
fn explicit_sort_shows_no_markers() {
    let t0 = Instant::now();
    let mut app = animated(Theme::no_color(), t0);
    let es = resort_fixture(&mut app, t0);
    let t1 = t0 + ms(100);
    app.refresh(snap(es, 2), t1);
    assert_eq!(names(&app), ["a", "b", "c"]);
    app.key(key(KeyCode::Char('S')), t1);
    app.reorder(t1);
    assert_eq!(names(&app), ["c", "a", "b"]);
    let frame = render(&mut app, t1, W, H);
    let rows = list_rows(&frame);
    assert!(!rows.contains('↑') && !rows.contains('↓'), "{frame}");
    run_until(&mut app, t1, t1 + Duration::from_secs(5));
    assert_eq!(app.motion.next_frame_at(), None);
}

#[test]
fn rank_markers_survive_a_resort_that_replaced_the_entries() {
    let t0 = Instant::now();
    let mut app = animated(Theme::no_color(), t0);
    let mut es = resort_fixture(&mut app, t0);
    // The re-sort comes with a snapshot whose entries are in another order (a rescan).
    es.reverse();
    let t1 = t0 + Duration::from_secs(3);
    app.refresh(snap(es, 2), t1);
    assert_eq!(names(&app), ["c", "a", "b"]);
    let rows = list_rows(&render(&mut app, t1, W, H));
    assert!(rows.contains("c  ↑2"), "{rows}");
    assert!(rows.contains("a  ↓1"), "{rows}");
    assert!(rows.contains("b  ↓1"), "{rows}");
}

#[test]
fn deleted_row_stays_as_placeholder_until_resort() {
    let t0 = Instant::now();
    let mut app = animated(Theme::no_color(), t0);
    let (a, c) = (test_entry(0, "a", 300), test_entry(2, "c", 100));
    let b = EntryData {
        mark: Mark::Deleting,
        ..test_entry(1, "b", 200)
    };
    app.refresh(snap(vec![a.clone(), b, c.clone()], 1), t0);
    render_buffer(&mut app, t0, W, H);

    let t1 = t0 + ms(100);
    app.refresh(snap(vec![a.clone(), c.clone()], 2), t1);
    assert_eq!(names(&app), ["a", "b", "c"]);
    assert!(app.snap.entries[app.rows[1]].ghost);
    let buf = render_buffer(&mut app, t1, W, H);
    let frame = text(&buf);
    assert!(frame.contains("b  deleted"), "{frame}");
    let at = style_at(&buf, "b  deleted");
    assert!(at.add_modifier.contains(Modifier::CROSSED_OUT), "{at:?}");
    // Kept while the snapshots go on without it.
    let t2 = t0 + ms(200);
    app.refresh(snap(vec![a.clone(), c.clone()], 3), t2);
    assert_eq!(names(&app), ["a", "b", "c"]);

    app.key(key(KeyCode::Char('S')), t2);
    app.reorder(t2);
    assert_eq!(names(&app), ["a", "c"]);
    app.refresh(snap(vec![a, c], 4), t0 + ms(300));
    assert_eq!(names(&app), ["a", "c"]);
    assert_eq!(app.snap.entries.len(), 2, "the ghost was added again");
}

#[test]
fn entry_gone_while_a_delete_ran_becomes_a_placeholder() {
    let t0 = Instant::now();
    let mut app = animated(Theme::no_color(), t0);
    let es = vec![test_entry(0, "a", 300), test_entry(1, "b", 200)];
    let mut before = snap(es.clone(), 1);
    before.deleting = Some((0, 1));
    app.refresh(before, t0);
    // Deleted before any snapshot showed it marked.
    app.refresh(snap(vec![es[0].clone()], 2), t0 + ms(100));
    assert_eq!(names(&app), ["a", "b"]);
    // Gone without a delete: no placeholder.
    let t1 = t0 + ms(200);
    app.key(key(KeyCode::Char('S')), t1);
    app.reorder(t1);
    app.refresh(snap(es.clone(), 3), t1);
    app.refresh(snap(vec![es[0].clone()], 4), t1 + ms(100));
    assert_eq!(names(&app), ["a"]);
}

fn has_rgb(buf: &Buffer) -> bool {
    buf.content.iter().any(|c| {
        [c.fg, c.bg]
            .into_iter()
            .any(|c| matches!(c, Color::Rgb(..)))
    })
}

#[test]
fn delete_fade_only_colours_at_truecolor() {
    let themes = [
        Theme::ansi16(),
        Theme::no_color(),
        dark(ColorLevel::Ansi256),
        dark(ColorLevel::TrueColor),
    ];
    for theme in themes {
        let truecolor = theme.level() == ColorLevel::TrueColor;
        let dim = theme.dim.fg;
        let t0 = Instant::now();
        let mut app = animated(theme, t0);
        let b = EntryData {
            mark: Mark::Deleting,
            ..test_entry(1, "b", 200)
        };
        app.refresh(snap(vec![test_entry(0, "a", 300), b], 1), t0);
        render_buffer(&mut app, t0, W, H);
        let t1 = t0 + ms(100);
        app.refresh(snap(vec![test_entry(0, "a", 300)], 2), t1);
        let mut fgs = Vec::new();
        for n in 0..=6 {
            let buf = render_buffer(&mut app, t1 + ms(n * 50), W, H);
            if !truecolor {
                assert!(!has_rgb(&buf), "{}", styled(&buf));
            }
            fgs.push(style_at(&buf, "b  deleted").fg);
        }
        if truecolor {
            assert_eq!(fgs[0], dim);
            assert_ne!(fgs[2], fgs[0]);
            assert_ne!(fgs[5], fgs[2]);
            // Done after 250 ms: the row stays faded.
            assert_eq!(fgs[5], fgs[6]);
        } else {
            assert!(fgs.iter().all(|&c| c == fgs[0]), "{fgs:?}");
        }
        assert_eq!(app.motion.next_frame_at(), None);
    }
}

/// Draws a frame at `now`, then each frame the motion asks for before `until`, and records the
/// start of each flash in `flashes`.
fn note_flashes(app: &mut App, mut now: Instant, until: Instant, flashes: &mut Vec<Instant>) {
    loop {
        render_buffer(app, now, W, H);
        if let Some(t) = app.motion.flash_at
            && flashes.last() != Some(&t)
        {
            flashes.push(t);
        }
        match app.motion.next_frame_at() {
            Some(t) if t < until => now = t,
            _ => return,
        }
    }
}

#[test]
fn flashes_are_rate_limited() {
    let t0 = Instant::now();
    let mut app = animated(dark(ColorLevel::TrueColor), t0);
    let mut es = resort_fixture(&mut app, t0);
    let mut flashes = Vec::new();

    let t1 = t0 + Duration::from_secs(3);
    app.refresh(snap(es.clone(), 2), t1);
    let t2 = t1 + Duration::from_secs(2);
    note_flashes(&mut app, t1, t2, &mut flashes);
    assert_eq!(flashes, [t1]);

    // The next automatic re-sort, 2 s later.
    es[1].size = 5000;
    app.refresh(snap(es.clone(), 3), t2);
    assert_eq!(names(&app), ["b", "c", "a"]);
    let t3 = t2 + ms(100);
    note_flashes(&mut app, t2, t3, &mut flashes);
    // The scan finishes 100 ms later: a re-sort at once, with markers but no flash.
    es[0].size = 9000;
    let mut done = snap(es, 4);
    done.busy = false;
    app.refresh(done, t3);
    assert_eq!(names(&app), ["a", "b", "c"]);
    note_flashes(&mut app, t3, t3 + Duration::from_secs(5), &mut flashes);
    assert!(app.motion.next_frame_at().is_none());
    assert_eq!(flashes, [t1, t2]);
}

#[test]
fn flash_colours_only_at_truecolor() {
    let themes = [
        Theme::ansi16(),
        Theme::no_color(),
        dark(ColorLevel::Ansi256),
        dark(ColorLevel::TrueColor),
    ];
    for theme in themes {
        let truecolor = theme.level() == ColorLevel::TrueColor;
        let t0 = Instant::now();
        let mut app = animated(theme, t0);
        let es = resort_fixture(&mut app, t0);
        let t1 = t0 + Duration::from_secs(3);
        app.refresh(snap(es, 2), t1);
        let buf = render_buffer(&mut app, t1, W, H);
        assert_eq!(has_rgb(&buf), truecolor, "{}", styled(&buf));
        let during = style_at(&buf, "a  ↓1");
        let after = style_at(&render_buffer(&mut app, t1 + FLASH, W, H), "a  ↓1");
        if truecolor {
            assert!(matches!(during.bg, Some(Color::Rgb(..))), "{during:?}");
            assert_ne!(during.bg, after.bg);
        } else {
            assert_ne!(
                during.add_modifier.contains(Modifier::REVERSED),
                after.add_modifier.contains(Modifier::REVERSED)
            );
        }
    }
}

#[test]
fn effects_skip_the_overlay() {
    let (w, h) = (100, 30);
    let t0 = Instant::now();
    let es: Vec<EntryData> = (0..30)
        .map(|i| test_entry(i, &format!("e{i:02}"), 1000 + i as u64 * 10))
        .collect();
    // Every row moves in the re-sort, so every row flashes.
    let reversed: Vec<EntryData> = es
        .iter()
        .map(|e| EntryData {
            size: 5000 - e.size,
            ..e.clone()
        })
        .collect();
    let mut frames = Vec::new();
    for on in [false, true] {
        let mut app = App::new(dark(ColorLevel::TrueColor), t0);
        if on {
            start(&mut app);
        }
        app.refresh(snap(es.clone(), 1), t0);
        render_buffer(&mut app, t0, w, h);
        let t1 = t0 + Duration::from_secs(3);
        app.refresh(snap(reversed.clone(), 2), t1);
        app.key(key(KeyCode::Char('?')), t1);
        frames.push(render_buffer(&mut app, t1, w, h));
        if on {
            assert!(app.motion.flash_at.is_some());
            let cover = app.areas.overlay.expect("help is open");
            let list = app.areas.list;
            let (plain, fx) = (&frames[0], &frames[1]);
            for y in list.top()..list.bottom() {
                for x in list.left()..list.right() {
                    if cover.contains((x, y).into()) {
                        assert_eq!(plain[(x, y)], fx[(x, y)], "({x}, {y})");
                    }
                }
            }
            // Left of the overlay the rows flash.
            let y = cover.top() + 1;
            assert_ne!(plain[(0, y)].bg, fx[(0, y)].bg);
        }
    }
}

#[test]
fn easing_sixty_rows_costs_under_two_ms_a_frame() {
    let t0 = Instant::now();
    let mut app = animated(dark(ColorLevel::TrueColor), t0);
    let mut es: Vec<EntryData> = (0..100)
        .map(|i| test_entry(i, &format!("entry{i:03}"), 1000 + i as u64 * 10))
        .collect();
    app.refresh(snap(es.clone(), 1), t0);
    render_buffer(&mut app, t0, 120, 64);
    assert_eq!(app.list_height, 60);
    // Reversed sizes: every row moves and flashes, and every bar eases.
    for (i, e) in es.iter_mut().enumerate() {
        e.size = 5000 - i as u64 * 10;
    }
    let t1 = t0 + Duration::from_secs(3);
    app.refresh(snap(es, 2), t1);
    let mut buf = render_buffer(&mut app, t1, 120, 64);
    assert_eq!(app.motion.markers.len(), 60);
    let visible: Vec<usize> = app.rows[app.scroll..app.scroll + 60].to_vec();
    let mut costs = Vec::new();
    for n in 1..=12 {
        let now = t1 + FRAME * n;
        let t = Instant::now();
        frame(&mut app, now, &visible);
        post_process(&mut app, &mut buf, now);
        costs.push(t.elapsed());
        assert!(app.motion.easing);
        assert!(app.motion.flash_at.is_some());
    }
    costs.sort();
    let median = costs[costs.len() / 2];
    println!("motion frame with 60 rows easing and flashing: {median:?} (median of 12)");
    let limit = if cfg!(debug_assertions) {
        ms(20)
    } else {
        ms(2)
    };
    assert!(median < limit, "{costs:?}");
}
