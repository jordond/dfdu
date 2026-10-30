use std::fs;
use std::path::Path;
use std::sync::{Condvar, Mutex};

use dfdu_scan::PortableBackend;
use dfdu_store::cache::{Cache, Key};

use super::*;
use crate::theme::Theme;

/// A directory with a few files in it, and the cache link for it, its cache in `cache` below
/// `tmp`.
fn link(tmp: &Path) -> CacheLink {
    let dir = tmp.join("scanned");
    fs::create_dir_all(dir.join("sub")).unwrap();
    fs::write(dir.join("a"), b"data").unwrap();
    fs::write(dir.join("sub/b"), vec![0u8; 5000]).unwrap();
    let cache = Cache::at(tmp.join("cache")).unwrap();
    CacheLink {
        slot: cache.slot(Key::new(&dir, false, false, false).unwrap()),
        path: dir,
        job: JobOptions::default(),
        save: true,
    }
}

/// A cacher for `link` that saves scans of any length and asks no change journal (its answers
/// take a while, and a full rescan is what is tested here).
fn cacher(link: &CacheLink) -> Cacher {
    let mut c = Cacher::new(Some(link.clone()));
    c.min_scan_ms = 0;
    c.position = |_| None;
    c
}

/// Polls until `done` holds, at most 10 s.
fn poll_until(
    c: &mut Cacher,
    app: &mut App,
    engine: &Engine<PortableBackend>,
    done: impl Fn(&Cacher, &App) -> bool,
) {
    let t0 = Instant::now();
    while !done(c, app) {
        assert!(t0.elapsed() < Duration::from_secs(10), "timed out");
        c.poll(app, engine);
        thread::sleep(Duration::from_millis(5));
    }
}

fn items(engine: &Engine<PortableBackend>) -> Option<u64> {
    engine.read(|m| m.tree.roots().next().map(|r| m.tree.total(r).items))
}

#[test]
fn ages_are_short() {
    assert_eq!(ago(100, 100), "0 s");
    assert_eq!(ago(100, 159), "59 s");
    assert_eq!(ago(0, 60), "1 min");
    assert_eq!(ago(0, 3 * 3_600 + 5), "3 h");
    assert_eq!(ago(0, 2 * 86_400), "2 d");
    assert_eq!(ago(200, 100), "0 s", "a time ahead of the clock");
}

/// The first run scans and saves the complete scan; the next opens it at once, says it is
/// cached while it is brought up to date, and then shows the scan as any other.
#[test]
fn a_complete_scan_is_saved_and_opened_next_time() {
    let tmp = tempfile::tempdir().unwrap();
    let link = link(tmp.path());
    let e = Engine::portable(2);
    let mut app = App::new(Theme::no_color(), Instant::now());
    let mut c = cacher(&link);
    assert!(c.poll(&mut app, &e), "the scan is sent at once");
    assert_eq!(app.cached, None, "nothing was cached");
    poll_until(&mut c, &mut app, &e, |c, _| {
        c.writer.is_none() && c.run.as_ref().is_some_and(|r| r.saved_as.is_some())
    });
    assert!(link.slot.path.exists());
    assert_eq!(app.status, None);
    let scanned = items(&e);
    drop(c);
    e.shutdown(Duration::from_millis(200));

    let e = Engine::portable(2);
    let mut app = App::new(Theme::no_color(), Instant::now());
    let mut c = cacher(&link);
    c.poll(&mut app, &e);
    assert_eq!(app.cached, Some(Cached::Loading));
    poll_until(&mut c, &mut app, &e, |c, _| c.loader.is_none());
    assert!(matches!(app.cached, Some(Cached::Refreshing { .. })));
    assert_eq!(items(&e), scanned, "the cached tree is shown");
    poll_until(&mut c, &mut app, &e, |_, app| app.cached.is_none());
    assert_eq!(items(&e), scanned);
    assert_eq!(app.status, None);
    drop(c);
    e.shutdown(Duration::from_millis(200));
}

/// A gate threads wait at until it is opened.
struct Gate {
    open: Mutex<bool>,
    opened: Condvar,
}

impl Gate {
    const fn new() -> Self {
        Gate {
            open: Mutex::new(false),
            opened: Condvar::new(),
        }
    }

    fn wait(&self) {
        let mut open = self.open.lock().unwrap();
        while !*open {
            open = self.opened.wait(open).unwrap();
        }
    }

    fn open(&self) {
        *self.open.lock().unwrap() = true;
        self.opened.notify_all();
    }
}

/// Holds the change journal's answer in the test below until opened.
static ANSWER: Gate = Gate::new();

/// A rescan sent while the resume waits for the change journal is refused (no resume has
/// taken the cached tree over yet), which leaves it not yet brought up to date: nothing is
/// saved until the resume has ended.
#[test]
fn nothing_is_saved_before_the_resume_ended() {
    const AT: JournalPosition = JournalPosition {
        volume: [1; 16],
        event_id: 7,
        time: 0,
    };
    let tmp = tempfile::tempdir().unwrap();
    let link = link(tmp.path());
    let e = Engine::portable(2);
    let mut app = App::new(Theme::no_color(), Instant::now());
    let mut c = cacher(&link);
    c.position = |_| Some(AT);
    poll_until(&mut c, &mut app, &e, |c, _| {
        c.writer.is_none() && c.run.as_ref().is_some_and(|r| r.saved_as.is_some())
    });
    let scanned = items(&e).unwrap();
    drop(c);
    e.shutdown(Duration::from_millis(200));
    let before = fs::read(&link.slot.path).unwrap();
    // Changed since, as the journal will say.
    fs::write(link.path.join("new"), b"new").unwrap();

    let e = Engine::portable(2);
    let mut app = App::new(Theme::no_color(), Instant::now());
    let mut c = cacher(&link);
    c.position = |_| Some(AT);
    c.changed_since = |_, _, _| {
        ANSWER.wait();
        Some(vec![PathBuf::new()])
    };
    c.poll(&mut app, &e);
    poll_until(&mut c, &mut app, &e, |c, _| c.loader.is_none());
    let sub = e.read(|m| {
        let r = m.tree.roots().next()?;
        let sub = m.tree.children(r).find(|&x| m.tree.name(x) == b"sub")?;
        Some(m.tree.node_ref(sub))
    });
    e.send(Command::Rescan { node: sub.unwrap() });
    let refused = |m: &Model| {
        m.status
            .notices
            .iter()
            .any(|n| n.starts_with("rescan: not refreshed yet"))
            && m.status.jobs.is_empty()
    };
    poll_until(&mut c, &mut app, &e, |_, _| e.read(refused));
    for _ in 0..20 {
        c.poll(&mut app, &e);
        thread::sleep(Duration::from_millis(2));
    }
    assert!(c.writer.is_none() && c.run.as_ref().is_some_and(|r| r.saved_as.is_none()));
    assert!(matches!(app.cached, Some(Cached::Refreshing { .. })));
    assert_eq!(fs::read(&link.slot.path).unwrap(), before);
    assert_eq!(
        items(&e),
        Some(scanned),
        "the cached tree, without the new file"
    );

    ANSWER.open();
    poll_until(&mut c, &mut app, &e, |c, _| {
        c.writer.is_none() && c.run.as_ref().is_some_and(|r| r.saved_as.is_some())
    });
    assert_eq!(app.cached, None);
    assert_eq!(items(&e), Some(scanned + 1));
    let hit = link.slot.load(cache::now()).unwrap().expect("saved");
    assert_eq!(
        hit.loaded.tree.total(hit.loaded.root).items,
        scanned + 1,
        "the tree saved is the resumed one"
    );
    drop(c);
    e.shutdown(Duration::from_millis(200));
}

/// A damaged cached scan is reported in the status line, removed, and the directory scanned.
#[test]
fn a_damaged_cache_is_reported_and_scanned_instead() {
    let tmp = tempfile::tempdir().unwrap();
    let link = link(tmp.path());
    fs::create_dir_all(link.slot.path.parent().unwrap()).unwrap();
    fs::write(&link.slot.path, b"DFDUSNAP\x04\0\0\0cut").unwrap();
    let e = Engine::portable(2);
    let mut app = App::new(Theme::no_color(), Instant::now());
    let mut c = cacher(&link);
    c.poll(&mut app, &e);
    poll_until(&mut c, &mut app, &e, |c, _| c.loader.is_none());
    let status = app.status.clone().expect("a notice");
    assert!(status.starts_with("cached scan "), "{status}");
    assert!(status.ends_with("; scanning"), "{status}");
    assert_eq!(app.cached, None);
    assert!(!link.slot.path.exists());
    assert!(e.wait_idle(Some(Duration::from_secs(10))).is_idle());
    assert_eq!(items(&e), Some(4));
    e.shutdown(Duration::from_millis(200));
}

/// A scan quicker than the minimum, or in a read-only browser, is not saved.
#[test]
fn quick_or_read_only_scans_are_not_saved() {
    let tmp = tempfile::tempdir().unwrap();
    for (min, save) in [(MIN_SCAN_MS, true), (0, false)] {
        let link = CacheLink {
            save,
            ..link(tmp.path())
        };
        let e = Engine::portable(2);
        let mut app = App::new(Theme::no_color(), Instant::now());
        let mut c = cacher(&link);
        c.min_scan_ms = min;
        poll_until(&mut c, &mut app, &e, |c, _| {
            c.run.as_ref().is_some_and(|r| r.saved_as.is_some())
        });
        assert!(c.writer.is_none());
        assert!(!c.busy());
        assert!(!link.slot.path.exists());
        drop(c);
        e.shutdown(Duration::from_millis(200));
    }
}

/// Measures opening a cached scan of about a million entries: how long the first poll and the
/// first frame take on the UI thread, when the cached tree is shown, and the longest poll.
/// `cargo test --release -p dfdu-tui --lib -- --ignored --nocapture measure_opening`.
#[test]
#[ignore = "a measurement, not a check"]
fn measure_opening_a_million() {
    use dfdu_tree::{Kind, Meta, Tree};
    let tmp = tempfile::tempdir().unwrap();
    let link = link(tmp.path());
    let root = link.slot.key.root();
    let meta = Meta::from_metadata(&fs::metadata(root).unwrap());
    let mut t = Tree::new();
    let r = t.new_root(root.as_os_str().as_encoded_bytes(), &meta, true);
    if let Some(d) = t.dir_mut(r) {
        d.state = DirState::Complete;
    }
    let mut ino = 10;
    for d in 0..10_000 {
        ino += 1;
        let dir = t.add_dir_flagged(
            r,
            format!("dir-{d:05}").as_bytes(),
            &Meta { ino, ..meta },
            0,
        );
        if let Some(x) = t.dir_mut(dir) {
            x.state = DirState::Complete;
        }
        let own = t.own(dir);
        t.propagate_add(r, &own);
        for f in 0..100 {
            ino += 1;
            let m = Meta {
                kind: Kind::File,
                apparent: f * 1000,
                alloc: 4096,
                ino,
                nlink: 1,
                ..meta
            };
            let x = t.add_file(dir, format!("file-{f:03}.dat").as_bytes(), &m, true, None);
            let own = t.own(x);
            t.propagate_add(dir, &own);
        }
    }
    let now = cache::now();
    let header = Header {
        tool_version: "test".into(),
        written: now - 3_600,
        cross_mounts: false,
        network: false,
        journal: dfdu_engine::journal_position(root),
        last_full_scan: now - 3_600,
        scan_ms: 60_000,
    };
    let saved = link
        .slot
        .save(&header, |f| f(&t), &AtomicBool::new(false))
        .unwrap();
    drop(t);
    let e = Engine::portable(2);
    let t0 = Instant::now();
    let mut app = App::new(Theme::no_color(), t0);
    let mut c = Cacher::new(Some(link.clone()));
    c.poll(&mut app, &e);
    let first_poll = t0.elapsed();
    let _ = crate::tests::render_buffer(&mut app, Instant::now(), 100, 30);
    let first_frame = t0.elapsed();
    let mut longest = Duration::ZERO;
    let mut shown = None;
    while app.cached.is_some() {
        assert!(t0.elapsed() < Duration::from_secs(60), "timed out");
        let p = Instant::now();
        c.poll(&mut app, &e);
        longest = longest.max(p.elapsed());
        if shown.is_none() && matches!(app.cached, Some(Cached::Refreshing { .. })) {
            shown = Some(t0.elapsed());
        }
        thread::sleep(Duration::from_millis(2));
    }
    eprintln!(
        "{saved:?}\nfirst poll {first_poll:?}, first frame {first_frame:?}, cached tree shown \
         {shown:?}, refreshed {:?}, longest poll {longest:?}",
        t0.elapsed()
    );
    drop(c);
    e.shutdown(Duration::from_millis(500));
}
