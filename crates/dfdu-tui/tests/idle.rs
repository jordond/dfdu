//! The run loop draws nothing while nothing changes: once a scan has finished and the loop has
//! caught up, no frame is flushed until a key arrives, and a key draws one frame. Animations
//! are on, as in `dfdu`, and the frames they draw stop when they end.
//!
//! This drives the real run loop (`dfdu_tui::event_loop_with`) and the real engine, drawing to a
//! `TestBackend` wrapped in a backend that records every flush; ratatui flushes once per
//! `Terminal::draw`.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use std::fs;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use common::Recording;
use crossbeam_channel::{Sender, unbounded};
use dfdu_engine::{Command, Engine, EngineOptions};
use dfdu_scan::JobOptions;
use dfdu_tui::{ColorLevel, IDLE_BEFORE_SORT, MIN_SORT_INTERVAL, Theme, UiOptions};
use ratatui::Terminal;
use ratatui::buffer::{Buffer, Cell};
use ratatui::crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};
use ratatui::style::Color;

/// How long without a frame counts as the loop having caught up with the finished scan. Longer
/// than any of its timed wakes: the spinner while busy and the automatic re-sort, which waits
/// for the longer of [`IDLE_BEFORE_SORT`] and [`MIN_SORT_INTERVAL`].
const QUIET: Duration = {
    let resort = if IDLE_BEFORE_SORT.as_nanos() > MIN_SORT_INTERVAL.as_nanos() {
        IDLE_BEFORE_SORT
    } else {
        MIN_SORT_INTERVAL
    };
    resort.saturating_add(Duration::from_millis(500))
};

#[derive(Default)]
struct Frames {
    at: Vec<Instant>,
    /// The first line of the last frame.
    header: String,
    /// The text of each frame, lines separated by `\n`.
    screens: Vec<String>,
    /// Whether each frame draws an RGB colour.
    rgb: Vec<bool>,
}

/// Frames flushed so far. The lock is released before an assertion can fail, so a failure does
/// not poison it for the run loop's thread.
fn count(frames: &Mutex<Frames>) -> usize {
    frames.lock().expect("frames").at.len()
}

/// Adds the frame in `buf` to `frames`.
fn record(frames: &Mutex<Frames>, buf: &Buffer) {
    let line = |y| -> String {
        (0..buf.area.width)
            .map(|x| buf.cell((x, y)).map_or(" ", Cell::symbol))
            .collect()
    };
    let header = line(0);
    let screen = (0..buf.area.height)
        .map(line)
        .collect::<Vec<_>>()
        .join("\n");
    let is_rgb = |c: Color| matches!(c, Color::Rgb(..));
    let rgb = buf.content.iter().any(|c| is_rgb(c.fg) || is_rgb(c.bg));
    let mut f = frames.lock().expect("frames");
    f.at.push(Instant::now());
    f.header = header;
    f.screens.push(screen);
    f.rgb.push(rgb);
}

/// Waits until no frame was flushed for [`QUIET`], at most until 10 s after `start`.
fn wait_quiet(frames: &Mutex<Frames>, start: Instant, what: &str) {
    loop {
        let last = frames.lock().expect("frames").at.last().copied();
        let now = Instant::now();
        if now.duration_since(last.unwrap_or(start).max(start)) >= QUIET {
            return;
        }
        assert!(
            now < start + Duration::from_secs(10),
            "the loop still draws 10 s after {what}: {} frames",
            count(frames)
        );
        thread::sleep(Duration::from_millis(10));
    }
}

/// [`settled_with`] the theme from the environment, as `dfdu_tui::event_loop` runs.
fn settled(check: impl FnOnce(Sender<Event>, &Mutex<Frames>)) {
    settled_with(UiOptions::from_env(), check);
}

/// Scans a small temporary tree with the run loop drawing it with `ui`, waits until the scan is
/// done and the loop has gone quiet, then hands the key sender and the recorded frames to
/// `check`. The loop ends when `check` drops the sender, also when it panics. The largest entry
/// is the file `big`, at the top of the list; the engine may delete.
fn settled_with(ui: UiOptions, check: impl FnOnce(Sender<Event>, &Mutex<Frames>)) {
    let temp_root = dunce::canonicalize(std::env::temp_dir()).expect("temp");
    let tmp = tempfile::tempdir_in(&temp_root).expect("tempdir");
    let journal = tempfile::tempdir_in(&temp_root).expect("journal dir");
    for d in 0..10 {
        let dir = tmp.path().join(format!("d{d}/sub"));
        fs::create_dir_all(&dir).expect("mkdir");
        for f in 0..30 {
            fs::write(dir.join(format!("f{f}")), vec![0u8; f * 100]).expect("write");
        }
    }
    fs::write(tmp.path().join("big"), vec![1u8; 4 << 20]).expect("write");
    let frames = Arc::new(Mutex::new(Frames::default()));
    let recorded = frames.clone();
    let mut terminal = Terminal::new(Recording::new(120, 20, move |buf| record(&recorded, buf)))
        .expect("terminal");
    let engine = Engine::start(
        dfdu_scan::native_backend(dfdu_sys::storage(tmp.path()), false),
        EngineOptions {
            workers: 2,
            journal_dir: Some(journal.path().to_path_buf()),
            crash_dir: None,
            ..Default::default()
        },
    );
    engine.send(Command::Scan {
        path: tmp.path().to_path_buf().into(),
        opts: JobOptions::default(),
    });
    thread::scope(|scope| {
        let (tx, rx) = unbounded();
        let run = scope.spawn(|| dfdu_tui::event_loop_with(&mut terminal, &engine, rx, ui));
        // Not `Engine::wait_idle`: it takes wake-ups from the channel the loop waits on.
        let start = Instant::now();
        loop {
            let header = frames.lock().expect("frames").header.clone();
            if header.contains("done in") {
                break;
            }
            assert!(
                start.elapsed() < Duration::from_secs(10),
                "the scan is not shown as done after 10 s: {header}"
            );
            thread::sleep(Duration::from_millis(10));
        }
        wait_quiet(&frames, Instant::now(), "the scan finished");
        let header = frames.lock().expect("frames").header.clone();
        assert!(header.contains("done in"), "{header}");
        check(tx, &frames);
        run.join().expect("run loop thread").expect("run loop");
    });
    engine.shutdown(Duration::from_millis(200));
}

#[test]
fn no_frames_after_scan_settles() {
    settled(|tx, frames| {
        let before = count(frames);
        thread::sleep(Duration::from_secs(2));
        assert_eq!(count(frames), before);
        drop(tx);
    });
}

#[test]
fn one_key_draws_one_frame() {
    settled(|tx, frames| {
        let before = count(frames);
        let sent = Instant::now();
        let key = KeyEvent::new(KeyCode::Char('j'), KeyModifiers::NONE);
        tx.send(Event::Key(key)).expect("send key");
        while count(frames) == before {
            assert!(
                sent.elapsed() < Duration::from_millis(500),
                "no frame for the key"
            );
            thread::sleep(Duration::from_millis(1));
        }
        thread::sleep(Duration::from_secs(2));
        assert_eq!(count(frames), before + 1);
        drop(tx);
    });
}

#[test]
fn no_frames_after_animations_finish() {
    settled(animations_finish);
}

/// At `TrueColor`, with a theme whose text colour is RGB, the deleted row also fades in colour,
/// a frame at a time; those frames end too.
#[test]
fn no_frames_after_colour_animations_finish() {
    let ui = UiOptions {
        theme: Theme::named("dark", ColorLevel::TrueColor, None).expect("dark"),
        color_level: ColorLevel::TrueColor,
        ..UiOptions::from_env()
    };
    settled_with(ui, animations_finish);
}

/// A loop started with the colourless `mono` theme on a `TrueColor` terminal resolves a theme
/// chosen in the palette at the terminal's colour level, not at mono's.
#[test]
fn palette_theme_after_mono_gets_the_terminal_colour_level() {
    let ui = UiOptions {
        theme: Theme::no_color(),
        color_level: ColorLevel::TrueColor,
        ..UiOptions::from_env()
    };
    settled_with(ui, |tx, frames| {
        assert!(
            !frames.lock().expect("frames").rgb.iter().any(|&rgb| rgb),
            "mono draws an RGB colour"
        );
        let before = count(frames);
        let press = |code| {
            let k = KeyEvent::new(code, KeyModifiers::NONE);
            tx.send(Event::Key(k)).expect("send key");
        };
        press(KeyCode::Char(':'));
        "dark".chars().for_each(|c| press(KeyCode::Char(c)));
        press(KeyCode::Enter);
        let sent = Instant::now();
        loop {
            let f = frames.lock().expect("frames");
            if f.rgb[before..].iter().any(|&rgb| rgb) {
                break;
            }
            let last = f.screens.last().cloned().unwrap_or_default();
            drop(f);
            assert!(
                sent.elapsed() < Duration::from_secs(2),
                "no RGB colour after switching to dark:\n{last}"
            );
            thread::sleep(Duration::from_millis(10));
        }
        drop(tx);
    });
}

/// Deletes the entry at the top of a settled browser, then checks that the animations that
/// brings (the placeholder, the re-sort that drops it, the bars that grow) draw nothing more
/// than 1 s after each step.
fn animations_finish(tx: Sender<Event>, frames: &Mutex<Frames>) {
    let before = count(frames);
    assert!(
        frames.lock().expect("frames").screens[before - 1].contains("  big"),
        "big is not shown"
    );
    // Delete the selected entry, `big`, for good: the placeholder stays in its row until
    // the next re-sort, and then every other bar grows.
    let press = |code| {
        let k = KeyEvent::new(code, KeyModifiers::NONE);
        tx.send(Event::Key(k)).expect("send key");
    };
    press(KeyCode::Char('D'));
    "delete".chars().for_each(|c| press(KeyCode::Char(c)));
    press(KeyCode::Enter);
    let sent = Instant::now();
    let summary = loop {
        let f = frames.lock().expect("frames");
        let found = f.screens[before..]
            .iter()
            .position(|s| s.contains("deleted 1 ("));
        if let Some(i) = found {
            break before + i;
        }
        let last = f.screens.last().cloned().unwrap_or_default();
        drop(f);
        assert!(
            sent.elapsed() < Duration::from_secs(10),
            "no delete summary:\n{last}"
        );
        thread::sleep(Duration::from_millis(10));
    };
    wait_quiet(frames, sent, "the delete");

    let f = frames.lock().expect("frames");
    let ghost = |i: usize| f.screens[i].contains("big  deleted");
    let first_ghost = (before..f.at.len()).find(|&i| ghost(i));
    let first_ghost = first_ghost.expect("no placeholder row for the deleted entry");
    let shown = summary.max(first_ghost);
    let resort = (shown..f.at.len())
        .find(|&i| !ghost(i))
        .expect("the placeholder stayed");
    let (at, n) = (f.at.clone(), f.at.len());
    drop(f);
    let late = |from: usize, to: usize| {
        (from..to)
            .filter(|&i| at[i] > at[from] + Duration::from_secs(1))
            .map(|i| at[i] - at[from])
            .collect::<Vec<_>>()
    };
    let after_delete = late(shown, resort);
    assert!(
        after_delete.is_empty(),
        "frames more than 1 s after the delete was shown: {after_delete:?}"
    );
    let after_resort = late(resort, n);
    assert!(
        after_resort.is_empty(),
        "frames more than 1 s after the re-sort: {after_resort:?}"
    );
    drop(tx);
}
