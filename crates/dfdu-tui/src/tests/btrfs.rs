//! What the browser shows on btrfs, on the fixture of `scripts/btrfs-fixture.sh` whose mount
//! point is in `DFDU_BTRFS_DIR` (and its DUP filesystem at `$DFDU_BTRFS_DIR-dup`). Skipped
//! (passing) when the variable is not set. Each test checks what its mode shows: unprivileged
//! and as root.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::MutexGuard;
use std::time::{Duration, Instant};

use dfdu_engine::{Command, Engine, EngineOptions};
use dfdu_scan::{JobOptions, NativeBackend};
use ratatui::crossterm::event::KeyCode;

use crate::app::{App, Effect, Overlay};
use crate::model::snapshot::{self, Target};
use crate::tests::{press, render};
use crate::theme::{Theme, human};

/// Tests run one at a time: one adds and removes a directory in the shared fixture.
fn fixture() -> Option<(PathBuf, MutexGuard<'static, ()>)> {
    static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    let dir = std::env::var_os("DFDU_BTRFS_DIR").map(PathBuf::from)?;
    Some((dir, LOCK.lock().unwrap_or_else(|e| e.into_inner())))
}

fn scan(root: &Path, journal: &Path) -> Engine<NativeBackend> {
    let e = Engine::start(
        NativeBackend::default(),
        EngineOptions {
            journal_dir: Some(journal.to_path_buf()),
            check_invariants: true,
            workers: 2,
            ..Default::default()
        },
    );
    e.send(Command::Scan {
        path: root.to_path_buf(),
        opts: JobOptions::default(),
    });
    assert!(e.wait_idle(Some(Duration::from_secs(30))).is_idle());
    e
}

/// The browser showing `dir`, drawn 160 columns wide.
fn show(e: &Engine<NativeBackend>, app: &mut App, dir: &Path, t: Instant) -> String {
    let mut target = e.read(|m| {
        let ix = snapshot::find_path(&m.tree, dir).expect("directory in the tree");
        Target::of(&m.tree, ix)
    });
    let snap = e.read(|m| snapshot::build(m, &mut target, app.size_mode));
    app.target = target;
    app.refresh(snap, t);
    render(app, t, 160, 24)
}

fn line_with<'a>(out: &'a str, name: &str) -> &'a str {
    out.lines()
        .find(|l| l.contains(name))
        .unwrap_or_else(|| panic!("{name} missing:\n{out}"))
}

/// Moves the cursor to the entry `name`.
fn select(app: &mut App, name: &str, t: Instant) {
    for _ in 0..app.rows.len() {
        if app
            .selected_entry()
            .is_some_and(|e| e.name[..] == *name.as_bytes())
        {
            return;
        }
        app.key(press(KeyCode::Down), t);
    }
    panic!("{name} not in the list");
}

/// Unprivileged: compressed files are labelled as upper bounds and the header says
/// so. As root: sizes are exact, so neither appears, and subvolumes show their quota groups.
#[test]
fn compressed_labels_and_quota_groups() {
    let Some((f, _lock)) = fixture() else { return };
    let j = tempfile::tempdir().unwrap();
    let e = scan(&f, j.path());
    let t = Instant::now();
    let mut app = App::new(Theme::no_color(), t);
    let top = show(&e, &mut app, &f, t);
    let header: String = top.lines().take(2).collect();
    let plain = show(&e, &mut app, &f.join("plain"), t);
    let text = line_with(&plain, "text.txt");
    let random = line_with(&plain, "random.bin");
    assert!(!random.contains("compressed"), "{plain}");
    let quota = e.read(|m| {
        let ix = snapshot::find_path(&m.tree, &f.join("sub")).unwrap();
        m.subvols[&m.tree.node_ref(ix)].qgroup
    });
    if dfdu_sys::is_privileged() {
        assert!(!header.contains("upper bound"), "{top}");
        assert!(!text.contains("compressed"), "{plain}");
        let (rfer, excl) = quota.expect("quota group as root");
        let want = format!(
            "[subvolume; quota: {} referenced, {} exclusive]",
            human(rfer),
            human(excl)
        );
        assert!(line_with(&top, "sub/").contains(&want), "{top}");
    } else {
        assert!(
            header.contains("compressed sizes are upper bounds (exact as root)"),
            "{top}"
        );
        assert!(text.contains("compressed: at most this"), "{plain}");
        assert_eq!(quota, None);
        assert!(line_with(&top, "sub/").contains("[subvolume]"), "{top}");
    }
    assert!(
        line_with(&top, "snap-ro/").contains("[snapshot of sub, read-only"),
        "{top}"
    );
    // A single-profile filesystem stores data once: no multiplier.
    assert!(!header.contains("data stored"), "{top}");
    e.shutdown(Duration::from_secs(1));
}

/// A snapshot scanned alone shares data with snapshots outside the scan. Its rows say how
/// much, and the delete dialog says the delete may free less.
#[test]
fn data_used_outside_the_scan_may_free_less() {
    let Some((f, _lock)) = fixture() else { return };
    let j = tempfile::tempdir().unwrap();
    let root = f.join("snap-rw");
    let e = scan(&root, j.path());
    let t = Instant::now();
    let mut app = App::new(Theme::no_color(), t);
    let out = show(&e, &mut app, &root, t);
    // a.bin's 8 MiB are shared with sub and snap-ro; only-here.bin is not shared.
    let shared = format!("{} also used outside the scan", human(8 << 20));
    assert!(line_with(&out, "a.bin").contains(&shared), "{out}");
    assert!(
        !line_with(&out, "only-here.bin").contains("outside"),
        "{out}"
    );
    select(&mut app, "a.bin", t);
    app.key(press(KeyCode::Char('D')), t);
    assert!(matches!(app.overlay, Overlay::ConfirmDelete(_)));
    let dialog = render(&mut app, t, 160, 24);
    let want = format!(
        "May free less: {} of it is also used outside the scan.",
        human(8 << 20)
    );
    assert!(dialog.contains(&want), "{dialog}");
    // A file that only this snapshot has frees all of it.
    app.key(press(KeyCode::Esc), t);
    select(&mut app, "only-here.bin", t);
    app.key(press(KeyCode::Char('D')), t);
    let dialog = render(&mut app, t, 160, 24);
    assert!(!dialog.contains("May free less"), "{dialog}");
    e.shutdown(Duration::from_secs(1));
}

/// After a delete on btrfs the status line says that btrfs frees the space when the
/// transaction commits.
#[test]
fn delete_on_btrfs_notes_when_space_is_freed() {
    let Some((f, _lock)) = fixture() else { return };
    let w = f.join(format!("work-tui-{}", std::process::id()));
    fs::create_dir(&w).unwrap();
    fs::write(w.join("junk"), vec![7u8; 1 << 20]).unwrap();
    let j = tempfile::tempdir().unwrap();
    let e = scan(&w, j.path());
    let t = Instant::now();
    let mut app = App::new(Theme::no_color(), t);
    show(&e, &mut app, &w, t);
    select(&mut app, "junk", t);
    app.key(press(KeyCode::Char('D')), t);
    for c in "delete".chars() {
        app.key(press(KeyCode::Char(c)), t);
    }
    let fx = app.key(press(KeyCode::Enter), t);
    let [Effect::Send(cmd)] = <[Effect; 1]>::try_from(fx).expect("one effect") else {
        panic!("expected a delete command");
    };
    e.send(cmd);
    assert!(e.wait_idle(Some(Duration::from_secs(30))).is_idle());
    let out = show(&e, &mut app, &w, t);
    e.shutdown(Duration::from_secs(1));
    let gone = !w.join("junk").exists();
    let _ = fs::remove_dir(&w);
    assert!(gone, "{out}");
    let status = line_with(&out, "deleted 1");
    assert!(
        status.contains("btrfs frees the space when it commits"),
        "{out}"
    );
}

/// On a filesystem with the DUP data profile the header states the multiplier.
#[test]
fn dup_profile_in_the_header() {
    let Some((f, _lock)) = fixture() else { return };
    let mut dup = f.into_os_string();
    dup.push("-dup");
    let dup = PathBuf::from(dup);
    let j = tempfile::tempdir().unwrap();
    let e = scan(&dup, j.path());
    let t = Instant::now();
    let mut app = App::new(Theme::no_color(), t);
    let out = show(&e, &mut app, &dup, t);
    e.shutdown(Duration::from_secs(1));
    let header: String = out.lines().take(2).collect();
    assert!(header.contains("data stored 2.0× (DUP)"), "{out}");
}
