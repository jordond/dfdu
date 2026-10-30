use super::*;
use std::sync::Mutex;

/// Held by each test: the last one checks that no sync directory of this process is left,
/// which another test's call in flight would break.
static ONE_AT_A_TIME: Mutex<()> = Mutex::new(());

/// Writes below a directory after a position show up as changes of the directories they were
/// made in.
#[test]
fn changes_after_an_id_are_reported() {
    let _one = ONE_AT_A_TIME
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().canonicalize().unwrap();
    std::fs::create_dir_all(root.join("a/b")).unwrap();
    std::fs::create_dir_all(root.join("quiet")).unwrap();
    std::fs::write(root.join("quiet/x"), b"x").unwrap();
    let dev = {
        use std::os::unix::fs::MetadataExt;
        std::fs::metadata(&root).unwrap().dev()
    };
    let since = position_event_id(dev);
    std::fs::write(root.join("a/b/new"), b"hello").unwrap();
    std::fs::write(root.join("a/top"), b"hi").unwrap();
    let mut got = Changes::default();
    // The daemon flushes events with some delay.
    for _ in 0..40 {
        got = changes_since(&root, since, Duration::from_secs(5)).unwrap();
        if got.dirs.contains(&root.join("a/b")) && got.dirs.contains(&root.join("a")) {
            break;
        }
        std::thread::sleep(Duration::from_millis(250));
    }
    assert!(got.dirs.contains(&root.join("a/b")), "{got:?}");
    assert!(got.dirs.contains(&root.join("a")), "{got:?}");
    assert!(got.last_id > since);
    assert!(volume_uuid(dev).is_some());
}

/// A change made just before the call is reported, without waiting or asking again, in a
/// directory created just before the position was taken ([`position_event_id`]).
#[test]
fn a_change_just_before_the_call_is_reported() {
    use std::os::unix::fs::MetadataExt;
    let _one = ONE_AT_A_TIME
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().canonicalize().unwrap();
    let dev = std::fs::metadata(&root).unwrap().dev();
    for i in 0..5 {
        let d = root.join(format!("d{i}"));
        std::fs::create_dir(&d).unwrap();
        // Not the bare current id: the write below could fold into the event the daemon
        // still holds for the new directory, with an id before it.
        let since = position_event_id(dev);
        std::fs::write(d.join("f"), b"x").unwrap();
        let got = changes_since(&root, since, Duration::from_secs(10)).unwrap();
        assert!(!got.full);
        assert!(got.dirs.contains(&d), "{i}: {got:?}");
    }
    let left: Vec<_> = std::fs::read_dir(std::env::temp_dir().canonicalize().unwrap())
        .unwrap()
        .flatten()
        .filter(|e| {
            let n = e.file_name();
            let n = n.to_string_lossy();
            n.starts_with(&format!("dfdu-fsevents-{}-", std::process::id()))
        })
        .collect();
    assert!(left.is_empty(), "sync directories are removed");
}

/// A cancelled call returns at once, as interrupted, and leaves no sync directory behind.
#[test]
fn a_cancelled_call_cleans_up() {
    let _one = ONE_AT_A_TIME
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().canonicalize().unwrap();
    let cancel = AtomicBool::new(true);
    let t = Instant::now();
    let got = changes_since_cancellable(&root, 1, Duration::from_secs(30), &cancel);
    assert_eq!(got.unwrap_err().kind(), io::ErrorKind::Interrupted);
    assert!(t.elapsed() < Duration::from_secs(5));
    let prefix = format!("dfdu-fsevents-{}-", std::process::id());
    let left = std::fs::read_dir(std::env::temp_dir().canonicalize().unwrap())
        .unwrap()
        .flatten()
        .any(|e| e.file_name().to_string_lossy().starts_with(&prefix));
    assert!(!left, "the sync directory is removed");
}

/// Changes made right after the position is taken, to directories written to just before,
/// are all replayed (`--ignored`; `DFDU_ROUNDS`, default 200). Run it with the machine
/// loaded (the workspace tests, or a few busy loops): with the bare `current_event_id` as
/// the position about one round in 60 lost a change there, the daemon having folded it
/// into the event it still held, with its older id.
#[test]
#[ignore = "a stress run, meant for a loaded machine"]
fn changes_right_after_a_position_are_replayed() {
    use std::os::unix::fs::MetadataExt;
    let _one = ONE_AT_A_TIME
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().canonicalize().unwrap();
    let dev = std::fs::metadata(&root).unwrap().dev();
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
        let since = position_event_id(dev);
        longest = longest.max(t.elapsed());
        all += t.elapsed();
        std::fs::remove_file(d.join("a/one")).unwrap();
        std::fs::create_dir(d.join("a/b/c")).unwrap();
        std::fs::write(d.join("a/b/c/new"), b"y").unwrap();
        let got = changes_since(&d, since, Duration::from_secs(10)).unwrap();
        if !got.full && !(got.dirs.contains(&d.join("a")) && got.dirs.contains(&d.join("a/b/c"))) {
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
