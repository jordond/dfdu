mod bars;
mod color;
mod markers;
mod placeholders;

use std::time::{Duration, Instant};

use super::*;
use crate::snapshot::test_entry;
use crate::tests::render_buffer;
use crate::theme::{ColorLevel, Theme};

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
fn snap_with_alloc(entries: Vec<EntryData>, version: u64) -> Snapshot {
    let mut s = crate::tests::snap_without_totals(entries, version);
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

/// Entries a, b, c (largest first), drawn; then c grows to the largest.
fn resort_fixture(app: &mut App, t0: Instant) -> Vec<EntryData> {
    let mut es = vec![
        test_entry(0, "a", 300),
        test_entry(1, "b", 200),
        test_entry(2, "c", 100),
    ];
    app.refresh(snap_with_alloc(es.clone(), 1), t0);
    render_buffer(app, t0, W, H);
    es[2].size = 1000;
    es
}
