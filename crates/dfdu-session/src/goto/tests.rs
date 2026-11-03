use std::path::PathBuf;

use dfdu_tree::Host;

use super::*;
use crate::act::{self, Ran};
use crate::snapshot::{EntryData, test_entry};
use crate::state::tests::browser;

fn unix(p: &str) -> HostPath {
    HostPath::new(Host::Unix, p.as_bytes().to_vec())
}

#[test]
fn typed_paths_resolve_from_the_directory_shown() {
    let (cur, root) = (unix("/mnt/cache/a"), unix("/mnt/cache"));
    let home = PathBuf::from("/home/u");
    let r = |input: &str, local: bool| resolve(input, &cur, &root, Some(&home), local);
    let within = |p: &str| Ok(Dest::Within(unix(p)));
    let outside = |p: &str| Ok(Dest::Outside(unix(p)));
    assert_eq!(r("b", false), within("/mnt/cache/a/b"));
    assert_eq!(r("./b/", false), within("/mnt/cache/a/b"));
    assert_eq!(r("..", false), within("/mnt/cache"));
    assert_eq!(r("../..", false), outside("/mnt"));
    assert_eq!(r("../../disk1/x/../y", false), outside("/mnt/disk1/y"));
    assert_eq!(r("/mnt/cache2", false), outside("/mnt/cache2"));
    assert_eq!(r("/../../..", false), outside("/"));
    assert_eq!(r("  /mnt/cache//a  ", false), within("/mnt/cache/a"));
    assert!(r("", false).is_err() && r("   ", false).is_err());
    assert!(r("~", false).is_err(), "no ~ on another host's scan");
    if Host::LOCAL == Host::Unix {
        assert_eq!(r("~", true), outside("/home/u"));
        assert_eq!(r("~/src/..", true), outside("/home/u"));
        assert_eq!(r("~x", true), within("/mnt/cache/a/~x"));
    }
}

#[test]
fn windows_paths_resolve_with_their_own_separators() {
    let p = |s: &str| HostPath::new(Host::Windows, s.as_bytes().to_vec());
    let (cur, root) = (p(r"C:\Users\u"), p(r"C:\Users"));
    let r = |input: &str| resolve(input, &cur, &root, None, false);
    assert_eq!(r(r"..\..\Windows"), Ok(Dest::Outside(p(r"C:\Windows"))));
    assert_eq!(r("docs/x"), Ok(Dest::Within(p(r"C:\Users\u\docs\x"))));
    assert_eq!(r(r"D:\"), Ok(Dest::Outside(p(r"D:\"))));
}

/// A fake host with `/mnt/{cache,disk1,disk2,.hidden}` and `/mnt/disk1/{a,b}`.
fn list(dir: &HostPath) -> io::Result<Vec<String>> {
    let names: &[&str] = match dir.as_bytes() {
        b"/mnt" => &["disk2", "cache", ".hidden", "disk1"],
        b"/mnt/disk1" => &["b", "a"],
        b"/" => &["mnt"],
        _ => return Err(io::Error::from(io::ErrorKind::NotFound)),
    };
    Ok(names.iter().map(|&n| n.to_owned()).collect())
}

#[test]
fn completion_extends_the_last_component_from_the_directories_there() {
    let cur = unix("/mnt");
    let c = |input: &str| complete(input, &cur, None, false, list);
    let done = |text: &str| Completion {
        text: Some(text.into()),
        candidates: Vec::new(),
    };
    assert_eq!(c("/mnt/c"), done("/mnt/cache/"));
    assert_eq!(c("/m"), done("/mnt/"));
    assert_eq!(
        c("disk1/"),
        Completion {
            text: None,
            candidates: vec!["a".into(), "b".into()],
        }
    );
    assert_eq!(
        c("/mnt/d"),
        Completion {
            text: Some("/mnt/disk".into()),
            candidates: vec!["disk1".into(), "disk2".into()],
        }
    );
    assert_eq!(
        c("/mnt/disk"),
        Completion {
            text: None,
            candidates: vec!["disk1".into(), "disk2".into()],
        }
    );
    assert_eq!(
        c("/mnt/"),
        Completion {
            text: None,
            candidates: vec!["cache".into(), "disk1".into(), "disk2".into()],
        }
    );
    assert_eq!(c("/mnt/."), done("/mnt/.hidden/"));
    assert_eq!(c("/mnt/x"), Completion::default());
    assert_eq!(c("/nowhere/x"), Completion::default());
    assert_eq!(
        c("dis"),
        Completion {
            text: Some("disk".into()),
            candidates: vec!["disk1".into(), "disk2".into()],
        }
    );
}

#[test]
fn completion_on_this_machine_lists_only_directories() {
    let tmp = tempfile::tempdir().expect("temp dir");
    std::fs::create_dir(tmp.path().join("alpha")).expect("mkdir");
    std::fs::write(tmp.path().join("alps"), b"").expect("write");
    let cur = HostPath::local(tmp.path());
    let c = complete("al", &cur, None, true, list_local);
    let sep = char::from(Host::LOCAL.sep());
    assert_eq!(c.text, Some(format!("alpha{sep}")));
}

/// A browser of the scan root `/r` with the directory `lib`, a scan of another host.
fn at_root() -> SessionState {
    let lib = EntryData {
        is_dir: true,
        ..test_entry(0, "lib", 100)
    };
    let mut s = browser(vec![lib]);
    s.snap.root_path = s.snap.dir_path.clone();
    s
}

fn reopen(ran: Ran) -> Option<HostPath> {
    match ran {
        Ran::Done(then) => then.reopen,
        Ran::FrontEnd => panic!("left to the front end"),
    }
}

#[test]
fn parent_at_the_root_asks_first_and_scans_the_parent_on_a_second_press() {
    let mut s = at_root();
    assert_eq!(reopen(act::run(&mut s, Action::Parent)), None);
    let status = s.status.clone().unwrap_or_default();
    assert!(status.contains("again to scan /"), "{status}");
    assert_eq!(
        reopen(act::run(&mut s, Action::Parent)),
        Some(HostPath::from("/"))
    );

    // Any other action in between disarms it.
    let mut s = at_root();
    let _ = reopen(act::run(&mut s, Action::Parent));
    let _ = reopen(act::run(&mut s, Action::Down));
    assert_eq!(reopen(act::run(&mut s, Action::Parent)), None);
    assert!(s.up_armed);
}

#[test]
fn parent_at_the_top_of_the_filesystem_or_of_a_file_opens_nothing() {
    let mut s = at_root();
    s.snap.root_path = HostPath::from("/");
    s.snap.dir_path = HostPath::from("/");
    for _ in 0..2 {
        assert_eq!(reopen(act::run(&mut s, Action::Parent)), None);
    }
    assert_eq!(
        s.status.as_deref(),
        Some("already at the top of the filesystem")
    );

    let mut s = at_root();
    s.from_file = true;
    for _ in 0..2 {
        assert_eq!(reopen(act::run(&mut s, Action::Parent)), None);
        assert_eq!(s.status.as_deref(), Some(FROM_FILE));
    }
}

#[test]
fn go_opens_a_directory_of_the_scan_or_returns_one_outside_it() {
    let mut s = at_root();
    match go(&mut s, "lib/x/..") {
        Ok(Gone::Within(Some(m))) => assert_eq!(m.opened, Some(HostPath::from("/r/lib"))),
        other => panic!("{other:?}"),
    }
    assert_eq!(s.target.path, HostPath::from("/r/lib"));
    assert!(matches!(go(&mut s, "/r"), Ok(Gone::Within(Some(_)))));
    match go(&mut s, "/elsewhere") {
        Ok(Gone::Outside(p)) => assert_eq!(p, HostPath::from("/elsewhere")),
        other => panic!("{other:?}"),
    }
    s.from_file = true;
    assert_eq!(go(&mut s, "/elsewhere").err().as_deref(), Some(FROM_FILE));
    assert!(go(&mut s, "").is_err());
}

#[test]
fn go_on_this_machine_says_what_is_not_a_folder() {
    let tmp = tempfile::tempdir().expect("temp dir");
    let root = dunce::canonicalize(tmp.path()).expect("canonical");
    std::fs::create_dir(root.join("d")).expect("mkdir");
    std::fs::write(root.join("f"), b"").expect("write");
    let mut s = at_root();
    s.outside.local = true;
    s.snap.root_path = HostPath::local(&root);
    s.snap.dir_path = HostPath::local(&root);
    let err = go(&mut s, "f").err().unwrap_or_default();
    assert!(err.starts_with("not a folder"), "{err}");
    let err = go(&mut s, "nope").err().unwrap_or_default();
    assert!(err.starts_with("no such folder"), "{err}");
    assert!(matches!(go(&mut s, "d"), Ok(Gone::Within(Some(_)))));
    // The parent is outside, and is a folder.
    assert!(matches!(go(&mut s, ".."), Ok(Gone::Outside(_))));
}
