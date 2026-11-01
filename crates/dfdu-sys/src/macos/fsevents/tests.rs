use super::*;
use std::sync::{Mutex, MutexGuard};

/// Held by each test: the last one checks that no sync directory of this process is left,
/// which another test's call in flight would break.
static ONE_AT_A_TIME: Mutex<()> = Mutex::new(());

fn one_at_a_time() -> MutexGuard<'static, ()> {
    ONE_AT_A_TIME
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// A temporary directory, and its path with symbolic links resolved, as the daemon names it.
fn temp_root() -> (tempfile::TempDir, PathBuf) {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().canonicalize().unwrap();
    (tmp, root)
}

/// The changes below `root` since `since`, asked again for a while until `seen` says they are
/// all there: the daemon flushes events with some delay, and a busy one can hold them past
/// one call. There is nothing to wait on but the next call, so the polls are spaced out.
fn changes_until(
    root: &Path,
    since: u64,
    timeout: Duration,
    seen: impl Fn(&Changes) -> bool,
) -> Changes {
    let mut got = changes_since(root, since, timeout).unwrap();
    for _ in 0..40 {
        if seen(&got) {
            break;
        }
        std::thread::sleep(Duration::from_millis(250));
        got = changes_since(root, since, timeout).unwrap();
    }
    got
}

/// Whether a sync directory of this process is left in the temporary directory.
fn sync_dirs_left() -> bool {
    let prefix = format!("dfdu-fsevents-{}-", std::process::id());
    std::fs::read_dir(std::env::temp_dir().canonicalize().unwrap())
        .unwrap()
        .flatten()
        .any(|e| e.file_name().to_string_lossy().starts_with(&prefix))
}

/// Writes below a directory after a position show up as changes of the directories they were
/// made in.
#[test]
#[ignore = "waits on the FSEvents daemon, which a busy machine holds up: `cargo xtask test --daemon` runs it alone, CI too"]
fn changes_after_an_id_are_reported() {
    let _one = one_at_a_time();
    let (_tmp, root) = temp_root();
    std::fs::create_dir_all(root.join("a/b")).unwrap();
    std::fs::create_dir_all(root.join("quiet")).unwrap();
    std::fs::write(root.join("quiet/x"), b"x").unwrap();
    let dev = std::fs::metadata(&root).unwrap().dev();
    let since = position_event_id();
    std::fs::write(root.join("a/b/new"), b"hello").unwrap();
    std::fs::write(root.join("a/top"), b"hi").unwrap();
    let (ab, a) = (root.join("a/b"), root.join("a"));
    let got = changes_until(&root, since, Duration::from_secs(5), |got| {
        got.dirs.contains(&ab) && got.dirs.contains(&a)
    });
    assert!(got.dirs.contains(&ab), "{got:?}");
    assert!(got.dirs.contains(&a), "{got:?}");
    assert!(got.last_id > since);
    assert!(volume_uuid(dev).is_some());
}

/// A change made just before the call is reported, in a directory created just before the
/// position was taken ([`position_event_id`]). A busy daemon can hold it past one call (the
/// resume's verify pass covers that), so the call is repeated for a while.
#[test]
#[ignore = "waits on the FSEvents daemon, which a busy machine holds up: `cargo xtask test --daemon` runs it alone, CI too"]
fn a_change_just_before_the_call_is_reported() {
    let _one = one_at_a_time();
    let (_tmp, root) = temp_root();
    for i in 0..5 {
        let d = root.join(format!("d{i}"));
        std::fs::create_dir(&d).unwrap();
        // Not the bare current id: the write below could fold into the event the daemon
        // still holds for the new directory, with an id before it.
        let since = position_event_id();
        std::fs::write(d.join("f"), b"x").unwrap();
        let got = changes_until(&root, since, Duration::from_secs(10), |got| {
            got.dirs.contains(&d)
        });
        assert!(!got.full);
        assert!(got.dirs.contains(&d), "{i}: {got:?}");
    }
    assert!(!sync_dirs_left(), "sync directories are removed");
}

/// A cancelled call returns at once, as interrupted, and leaves no sync directory behind.
#[test]
fn a_cancelled_call_cleans_up() {
    let _one = one_at_a_time();
    let (_tmp, root) = temp_root();
    let cancel = AtomicBool::new(true);
    let t = Instant::now();
    let got = changes_since_cancellable(&root, 1, Duration::from_secs(30), &cancel);
    assert_eq!(got.unwrap_err().kind(), io::ErrorKind::Interrupted);
    assert!(t.elapsed() < Duration::from_secs(5));
    assert!(!sync_dirs_left(), "the sync directory is removed");
}

/// Changes made right after the position is taken, to directories written to just before,
/// are all replayed (`--ignored`; `DFDU_ROUNDS`, default 200). Run it with the machine
/// loaded (the workspace tests, or a few busy loops): with the bare `current_event_id` as
/// the position about one round in 60 lost a change there, the daemon having folded it
/// into the event it still held, with its older id.
#[test]
#[ignore = "a stress run, meant for a loaded machine"]
fn changes_right_after_a_position_are_replayed() {
    let _one = one_at_a_time();
    let (_tmp, root) = temp_root();
    let rounds = std::env::var("DFDU_ROUNDS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(200);
    let (mut lost, mut longest, mut all) = (Vec::new(), Duration::ZERO, Duration::ZERO);
    for i in 0..rounds {
        let d = root.join(format!("t{i}"));
        std::fs::create_dir_all(d.join("a/b")).unwrap();
        std::fs::write(d.join("a/one"), b"x").unwrap();
        let t = Instant::now();
        let since = position_event_id();
        longest = longest.max(t.elapsed());
        all += t.elapsed();
        std::fs::remove_file(d.join("a/one")).unwrap();
        std::fs::create_dir(d.join("a/b/c")).unwrap();
        std::fs::write(d.join("a/b/c/new"), b"y").unwrap();
        let got = changes_since(&d, since, Duration::from_secs(10)).unwrap();
        let replayed = got.dirs.contains(&d.join("a")) && got.dirs.contains(&d.join("a/b/c"));
        if !got.full && !replayed {
            lost.push((i, got));
        }
    }
    eprintln!("position: longest {longest:?}, mean {:?}", all / rounds);
    assert!(
        lost.is_empty(),
        "{} of {rounds} lost a change: {lost:?}",
        lost.len()
    );
}
