use std::time::Instant;

use dfdu_engine::wire::ListEntry;
use dfdu_tree::HostPath;

use super::*;

fn browser(dir: &str) -> SessionState {
    let mut s = SessionState::new(Instant::now());
    s.snap.dir_path = HostPath::from(dir);
    s.snap.root_path = HostPath::from(dir);
    s
}

#[test]
fn the_mounts_are_wanted_for_nothing_typed_or_the_root() {
    for (input, unix, windows) in [
        ("", true, true),
        ("  ", true, true),
        ("/", true, true),
        ("//", true, true),
        ("C:", false, true),
        ("c:\\", false, true),
        ("\\", false, true),
        ("/mnt", false, false),
        ("C:\\Users", false, false),
        ("disk", false, false),
    ] {
        assert_eq!(wants_mounts(input, Host::Unix), unix, "{input:?} on Unix");
        assert_eq!(
            wants_mounts(input, Host::Windows),
            windows,
            "{input:?} on Windows"
        );
    }
}

#[test]
fn a_tab_on_this_machine_completes_at_once() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::create_dir(tmp.path().join("disk1")).unwrap();
    let dir = HostPath::local(tmp.path());
    let mut s = browser("/");
    s.snap.dir_path = dir.clone();
    s.outside.local = true;
    let sep = char::from(dir.host().sep());
    let Some(Offer::Completed(c)) = tab(&mut s, &format!("{dir}{sep}d")) else {
        panic!("answered at once");
    };
    assert_eq!(c.text, Some(format!("{dir}{sep}disk1{sep}")));
    assert!(!s.goto.pending());
}

#[test]
fn a_tab_before_the_root_is_shown_asks_for_nothing() {
    // No folder is shown yet: a relative path would name one in dfdu's own working
    // directory (the crate's, under the test runner, which has `src`).
    let mut s = browser("");
    s.outside.local = true;
    for input in ["src/", "src/go", "disk"] {
        assert_eq!(
            tab(&mut s, input),
            Some(Offer::Completed(Completion::default())),
            "{input:?}"
        );
        assert!(!s.goto.pending());
    }
}

#[test]
fn a_scan_read_from_a_file_lists_nothing() {
    let mut s = browser("/r");
    assert_eq!(
        tab(&mut s, "/r/a"),
        Some(Offer::Completed(Completion::default()))
    );
    edited(&mut s, "/");
    assert!(!s.goto.pending(), "no host to ask for mounts");
}

/// A browser waiting for the listing of `/mnt` for the text `/mnt/d`, and the sender of its
/// answer.
fn waiting_for_list(s: &mut SessionState) -> crossbeam_channel::Sender<ListAnswer> {
    let (tx, rx) = bounded(1);
    let query = Query::of("/mnt/d", &s.snap.dir_path, None, false).unwrap();
    s.goto.waiting = Some(Waiting::List {
        input: "/mnt/d".into(),
        query,
        rx,
    });
    tx
}

#[test]
fn a_listing_that_arrives_completes_the_text_it_was_asked_for() {
    let mut s = browser("/r");
    let tx = waiting_for_list(&mut s);
    assert!(!s.goto.poll() && s.goto.pending());
    let entry = |name: &[u8], dir| ListEntry {
        name: name.to_vec(),
        dir,
    };
    tx.send(Ok(vec![
        entry(b"disk1", true),
        entry(b"disk2", true),
        entry(b"data", false),
        entry(b"d\xff", true),
    ]))
    .unwrap();
    assert!(s.goto.poll());
    assert!(!s.goto.pending());
    assert_eq!(take_answer(&mut s, "/mnt/x"), None, "another text drops it");
    let tx = waiting_for_list(&mut s);
    tx.send(Ok(vec![entry(b"disk1", true), entry(b"disk2", true)]))
        .unwrap();
    assert!(s.goto.poll());
    assert_eq!(
        take_answer(&mut s, "/mnt/d"),
        Some(Offer::Completed(Completion {
            text: Some("/mnt/disk".into()),
            candidates: vec!["disk1".into(), "disk2".into()],
        }))
    );
    assert_eq!(take_answer(&mut s, "/mnt/d"), None, "taken once");
}

#[test]
fn a_failed_or_lost_ask_says_why() {
    let mut s = browser("/r");
    let tx = waiting_for_list(&mut s);
    tx.send(Err("permission denied".into())).unwrap();
    assert!(s.goto.poll());
    assert_eq!(
        take_answer(&mut s, "/mnt/d"),
        Some(Offer::Failed("permission denied".into()))
    );
    drop(waiting_for_list(&mut s));
    assert!(s.goto.poll());
    assert_eq!(
        take_answer(&mut s, "/mnt/d"),
        Some(Offer::Failed(NO_ANSWER.into()))
    );
    // An edit forgets what was asked.
    let _tx = waiting_for_list(&mut s);
    edited(&mut s, "/mnt/da");
    assert!(!s.goto.pending());
}

#[test]
fn mounts_arrive_with_their_type_and_space() {
    let mut s = browser("/r");
    let (tx, rx) = bounded(1);
    s.goto.waiting = Some(Waiting::Mounts {
        input: "/".into(),
        rx,
    });
    let mount = |point: &[u8], used, total| HostMount {
        point: point.to_vec(),
        fstype: "xfs".into(),
        used,
        total,
    };
    tx.send(Ok(vec![
        mount(b"/", Some(1 << 30), Some(4 << 30)),
        mount(b"/mnt/\xff", None, None),
        mount(b"/mnt/disk1", None, None),
    ]))
    .unwrap();
    assert!(s.goto.poll());
    let Some(Offer::Mounts(m)) = take_answer(&mut s, "/") else {
        panic!("mounts");
    };
    assert_eq!(m.len(), 2, "a mount point that cannot be typed is left out");
    assert_eq!(m[0].note(), "xfs · 1.0 GiB of 4.0 GiB");
    assert_eq!(m[0].text(Host::Unix), "/");
    assert_eq!(m[1].note(), "xfs");
    assert_eq!(m[1].text(Host::Unix), "/mnt/disk1/");
    let drive = Mount {
        point: "D:\\".into(),
        ..m[1].clone()
    };
    assert_eq!(drive.text(Host::Windows), "D:\\");
}

#[test]
fn this_machines_mounts_are_read_on_a_thread() {
    let mut s = browser("/r");
    s.outside.local = true;
    assert_eq!(tab(&mut s, ""), None, "answered later");
    assert!(s.goto.pending());
    let until = Instant::now() + std::time::Duration::from_secs(10);
    while !s.goto.poll() {
        assert!(Instant::now() < until, "the mounts were not read");
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    assert!(matches!(
        take_answer(&mut s, ""),
        Some(Offer::Mounts(_) | Offer::Failed(_))
    ));
}
