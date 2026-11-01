use std::time::{Duration, Instant};

use dfdu_engine::{Engine, SizeMode};
use dfdu_scan::PortableBackend;

use super::pacing::{DATA_FRAME, Pacing};
use super::*;
use crate::snapshot::test_entry;
use crate::state::IDLE_BEFORE_SORT;
use crate::state::tests::browser;

const MS: Duration = Duration::from_millis(1);

/// A browser of `/r` holding two entries, read for the directory it shows, and the pacing
/// after a read of scan data at `t0` with more data come in since.
fn read_at(t0: Instant) -> (SessionState, Pacing) {
    let mut s = browser(vec![test_entry(0, "a", 10), test_entry(1, "b", 5)]);
    s.target.path = s.snap.dir_path.clone();
    let mut p = Pacing::new(SizeMode::Disk);
    p.last_read = Some(t0);
    p.shown(&s, t0);
    p.data_dirty = true;
    (s, p)
}

#[test]
fn scan_data_is_read_at_most_every_100_ms_or_nine_times_the_last_read() {
    let t0 = Instant::now();
    let (s, mut p) = read_at(t0);
    assert_eq!(p.read_due(&s, false, t0 + 99 * MS), Read::Nothing);
    assert_eq!(p.read_due(&s, false, t0 + 100 * MS), Read::Full);
    // A read that held the lock 50 ms waits 450 ms for the next.
    p.snap_cost = 50 * MS;
    assert_eq!(p.read_due(&s, false, t0 + 449 * MS), Read::Nothing);
    assert_eq!(p.read_due(&s, false, t0 + 450 * MS), Read::Full);
    // Until a frame showed entries, data is read as soon as it comes in.
    p.shown_entries = false;
    assert_eq!(p.read_due(&s, false, t0 + MS), Read::Full);
}

#[test]
fn another_directory_size_mode_or_breakdown_is_read_at_once() {
    let t0 = Instant::now();
    let (mut s, mut p) = read_at(t0);
    p.data_dirty = false;
    assert_eq!(p.read_due(&s, false, t0 + MS), Read::Nothing);
    assert_eq!(
        p.read_due(&s, true, t0 + MS),
        Read::Full,
        "the type picker shows the breakdown"
    );
    s.size_mode = SizeMode::Apparent;
    assert_eq!(p.read_due(&s, false, t0 + MS), Read::Full);
    s.size_mode = SizeMode::Disk;
    s.target.path = s.snap.dir_path.join(b"a");
    assert_eq!(p.read_due(&s, false, t0 + MS), Read::Full);
}

#[test]
fn the_wake_is_the_first_of_data_spinner_sort_and_polling() {
    let t0 = Instant::now();
    let (mut s, mut p) = read_at(t0);
    let now = t0 + 50 * MS;
    assert_eq!(p.wake_at(&s, false, false, now), Some(t0 + 100 * MS));
    p.data_dirty = false;
    assert_eq!(
        p.wake_at(&s, false, false, now),
        None,
        "nothing to wait for"
    );
    assert_eq!(p.wake_at(&s, false, true, now), Some(now + DATA_FRAME));
    // A spinner moves every 100 ms from the last frame shown.
    s.snap.busy = true;
    assert_eq!(p.wake_at(&s, false, false, now), Some(t0 + DATA_FRAME));
    assert_eq!(p.wake_at(&s, true, false, now), Some(now + DATA_FRAME));
    assert!(!p.spinner_due(&s, t0 + 99 * MS));
    assert!(p.spinner_due(&s, t0 + 100 * MS));
    s.snap.busy = false;
    // A re-sort waits for the user to be idle.
    s.sort_schedule.note_changed();
    s.note_input(t0 + 10 * MS);
    let due = s.next_sort_at().expect("a re-sort is pending");
    assert!(due >= t0 + 10 * MS + IDLE_BEFORE_SORT, "{due:?}");
    assert_eq!(p.wake_at(&s, false, false, now), Some(due));
}

/// A session on an engine that scans nothing, ticked a frame apart from `t0` (and a few ms apart
/// in real time) until two ticks in a row find nothing to do. Returns it with its clock then.
fn settled(
    engine: &Engine<PortableBackend>,
    t0: Instant,
) -> (Session<'_, PortableBackend>, Instant) {
    let mut session = Session::over(engine, SessionState::new(t0), SessionOptions::default());
    let tick = session.tick(t0).expect("no fatal error");
    assert!(tick.changed, "the first tick reads the model");
    session.shown(t0);
    let (mut now, mut quiet) = (t0, 0);
    while quiet < 2 {
        assert!(now < t0 + Duration::from_secs(100), "never settled");
        std::thread::sleep(5 * MS);
        now += DATA_FRAME;
        let tick = session.tick(now).expect("tick");
        if tick == Tick::default() {
            quiet += 1;
        } else {
            quiet = 0;
            session.shown(now);
        }
    }
    (session, now)
}

#[test]
fn the_automatic_re_sort_waits_for_its_deadline() {
    let engine = Engine::portable(1);
    let (mut session, t0) = settled(&engine, Instant::now());
    // A snapshot that changed came in, and the user pressed a key a second later.
    session.state_mut().sort_schedule.note_changed();
    session.state_mut().note_input(t0 + Duration::from_secs(1));
    let due = t0 + Duration::from_secs(1) + IDLE_BEFORE_SORT;
    let tick = session.tick(t0 + Duration::from_secs(2)).expect("tick");
    assert_eq!(
        tick,
        Tick {
            changed: false,
            wake_at: Some(due)
        }
    );
    let tick = session.tick(due).expect("tick");
    assert!(tick.changed, "sorted at the deadline");
    assert!(!session.state().sort_pending());
    assert_eq!(tick.wake_at, None);
    session.shutdown(Duration::ZERO);
    engine.shutdown(Duration::from_millis(200));
}

#[test]
fn a_scan_is_read_until_it_settles() {
    let tmp = tempfile::tempdir().expect("tempdir");
    std::fs::create_dir(tmp.path().join("d")).expect("mkdir");
    std::fs::write(tmp.path().join("d/f"), b"data").expect("write");
    let engine = Engine::portable(2);
    let (mut session, t0) = settled(&engine, Instant::now());
    engine.send(dfdu_engine::Command::Scan {
        path: tmp.path().to_path_buf().into(),
        opts: dfdu_scan::JobOptions::default(),
    });
    // The ticks' clock runs ahead of the scan's: each tick is a frame later, a few ms apart.
    let mut now = t0;
    let mut settled = false;
    let start = Instant::now();
    while start.elapsed() < Duration::from_secs(20) {
        now += DATA_FRAME;
        let tick = session.tick(now).expect("tick");
        if tick.changed {
            session.shown(now);
        } else if tick.wake_at.is_none() && !session.state().snap.entries.is_empty() {
            settled = true;
            break;
        }
        std::thread::sleep(2 * MS);
    }
    assert!(settled, "nothing left to wake for");
    let s = session.state();
    assert!(!s.snap.busy);
    assert_eq!(s.snap.entries.len(), 1, "{:?}", s.snap.entries);
    assert_eq!(&s.snap.entries[0].name[..], b"d");
    session.shutdown(Duration::ZERO);
    engine.shutdown(Duration::from_millis(200));
}

#[test]
fn shutting_down_stops_the_comparison_of_duplicates() {
    let engine = Engine::portable(1);
    let mut s = SessionState::new(Instant::now());
    s.view = crate::view::View::Duplicates;
    let run = crate::dupes::Run {
        root: "/r".into(),
        progress: std::sync::Arc::default(),
        cancel: std::sync::Arc::default(),
        result: std::sync::Arc::default(),
    };
    let cancel = run.cancel.clone();
    s.dupes.run = Some(run);
    let session = Session::over(&engine, s, SessionOptions::default());
    assert!(!cancel.load(std::sync::atomic::Ordering::Relaxed));
    session.shutdown(Duration::ZERO);
    assert!(cancel.load(std::sync::atomic::Ordering::Relaxed));
    engine.shutdown(Duration::from_millis(200));
}
