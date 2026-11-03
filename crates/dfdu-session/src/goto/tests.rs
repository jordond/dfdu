use std::path::PathBuf;

use dfdu_tree::Host;

use super::*;

mod go;

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
        // `\` is part of a name on Unix, not a separator after `~`.
        assert_eq!(r(r"~\src", true), within(r"/mnt/cache/a/~\src"));
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

#[test]
fn windows_paths_match_the_tree_whatever_their_separators_and_case() {
    let p = |s: &str| HostPath::new(Host::Windows, s.as_bytes().to_vec());
    let (cur, root) = (p(r"C:\Users\u"), p(r"C:\Users"));
    let r = |input: &str| resolve(input, &cur, &root, None, false);
    let within = |s: &str| Ok(Dest::Within(p(s)));
    assert_eq!(r("C:/Users/x"), within(r"C:\Users\x"));
    assert_eq!(r(r"c:\users"), within(r"C:\Users"));
    assert_eq!(r(r"c:\USERS\u\Docs"), within(r"C:\Users\u\Docs"));
    assert_eq!(r("c:/"), Ok(Dest::Outside(p(r"C:\"))));
    // One separator: the root of the drive shown, not a folder below it.
    assert_eq!(r(r"\Windows"), Ok(Dest::Outside(p(r"C:\Windows"))));
    assert_eq!(r("/users/u"), within(r"C:\Users\u"));
    // A share keeps its prefix.
    let share = p(r"\\srv\data\a");
    let on_share = resolve(r"\b", &share, &share, None, false);
    assert_eq!(on_share, Ok(Dest::Outside(p(r"\\srv\data\b"))));
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

#[test]
fn the_entry_come_from_is_the_first_name_below_the_new_root() {
    let name = |from: &str, root: &str| came_from_name(&unix(from), &unix(root));
    assert_eq!(name("/mnt/cache", "/mnt"), Some(b"cache".to_vec()));
    assert_eq!(name("/mnt/cache/a", "/"), Some(b"mnt".to_vec()));
    assert_eq!(name("/mnt", "/mnt"), None);
    assert_eq!(name("/srv/x", "/mnt"), None);
    let win = |p: &str| HostPath::new(Host::Windows, p.as_bytes().to_vec());
    assert_eq!(
        came_from_name(&win(r"c:\users\Alice"), &win(r"C:\Users")),
        Some(b"Alice".to_vec()),
        "a Windows host's names compare without case"
    );
}

#[test]
fn an_empty_root_is_one_whose_scan_finished_with_nothing() {
    let mut s = SessionState::new(std::time::Instant::now());
    s.snap.dir_path = unix("/mnt/cache");
    s.snap.root_path = unix("/mnt/cache");
    assert!(!empty_root(&s), "nothing read yet");
    s.snap.dir_node = Some(crate::snapshot::test_entry(1, "cache", 0).node);
    assert!(empty_root(&s));
    s.snap.busy = true;
    assert!(!empty_root(&s), "still scanning");
    s.snap.busy = false;
    s.snap.dir_mark = Some(Mark::Error);
    assert!(!empty_root(&s), "could not be read");
    s.snap.dir_mark = None;
    s.snap.dir_path = unix("/mnt/cache/a");
    assert!(!empty_root(&s), "below the root");
}
