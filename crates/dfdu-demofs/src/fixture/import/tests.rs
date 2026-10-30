use super::*;
#[cfg(unix)]
use crate::test_support::{names, read_all};
#[cfg(unix)]
use crate::{Fixture, Kind};

#[test]
fn globs_match_any_run_of_characters() {
    assert!(matches_glob("lib*.so.6", "libc.so.6"));
    assert!(matches_glob("lib*.so.6", "lib.so.6"));
    assert!(matches_glob("*-linux-*", "x86_64-linux-gnu"));
    assert!(matches_glob("*", "anything"));
    assert!(matches_glob("exact", "exact"));
    assert!(matches_glob("a*b*c", "aXbYbc"));
    assert!(!matches_glob("lib*.so.6", "libc.so.7"));
    assert!(!matches_glob("exact", "exactly"));
    assert!(!matches_glob("a*b*c", "acb"));
    // The prefix and suffix may not overlap.
    assert!(!matches_glob("ab*ba", "aba"));
}

#[test]
fn a_plain_path_expands_to_itself() {
    assert_eq!(expand("/no/such/path").unwrap(), ["/no/such/path"]);
}

#[cfg(unix)]
#[test]
fn imports_copy_files_links_and_directories() {
    use std::os::unix::fs::PermissionsExt as _;
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("sys");
    std::fs::create_dir_all(src.join("lib")).unwrap();
    std::fs::write(src.join("tool"), b"#!/bin/sh\n").unwrap();
    std::fs::set_permissions(src.join("tool"), std::fs::Permissions::from_mode(0o755)).unwrap();
    std::fs::write(src.join("lib/b.so"), b"b").unwrap();
    std::fs::write(src.join("lib/a.so"), b"a").unwrap();
    std::os::unix::fs::symlink("a.so", src.join("lib/a.so.1")).unwrap();
    let text = format!(
        "now = \"2026-10-01\"\nage = \"5d\"\nuid = 0\nimport = [{:?}, {:?}]\n",
        src.join("tool").display(),
        src.join("lib").display()
    );
    let f = Fixture::parse(&text).unwrap();
    let t = f.build().unwrap();
    let at = |p: &str| {
        let full = src.join(p);
        t.resolve(full.to_str().unwrap()).unwrap()
    };
    let tool = at("tool");
    assert_eq!(read_all(&t, tool), b"#!/bin/sh\n");
    assert_eq!(t.nodes[tool].mode, 0o755);
    assert_eq!(t.nodes[tool].uid, 0);
    assert_eq!(t.nodes[tool].mtime, f.now().unwrap() - 5 * 86_400);
    assert!(matches!(&t.nodes[at("lib/a.so.1")].kind, Kind::Symlink(b) if b == b"a.so"));
    assert_eq!(names(&t, at("lib")), [&b"a.so"[..], b"a.so.1", b"b.so"]);
    assert_eq!(read_all(&t, at("lib/b.so")), b"b");
    // A path that is not there is an error, not an empty import.
    let missing = format!(
        "now = \"2026-10-01\"\nimport = [{:?}]\n",
        src.join("gone").display()
    );
    assert!(Fixture::parse(&missing).unwrap().build().is_err());
}

#[cfg(unix)]
#[test]
fn import_patterns_take_every_match_and_fail_on_none() {
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("sys");
    for arch in ["aarch64-linux-gnu", "x86_64-linux-gnu", "other"] {
        std::fs::create_dir_all(src.join(arch)).unwrap();
        std::fs::write(src.join(arch).join("libc.so.6"), arch).unwrap();
    }
    let pattern = |p: &str| format!("{}/{p}", src.display());
    assert_eq!(
        expand(&pattern("*-linux-*/lib*.so.6")).unwrap(),
        [
            pattern("aarch64-linux-gnu/libc.so.6"),
            pattern("x86_64-linux-gnu/libc.so.6")
        ]
    );
    assert!(expand(&pattern("*-linux-gnu/libm.so.6")).is_err());
    let build = |p: &str| {
        let text = format!("now = \"2026-10-01\"\nimport = [{:?}]\n", pattern(p));
        Fixture::parse(&text).unwrap().build()
    };
    let t = build("*-linux-*/lib*.so.6").unwrap();
    let at = |p: &str| t.resolve(src.join(p).to_str().unwrap());
    assert_eq!(
        read_all(&t, at("aarch64-linux-gnu/libc.so.6").unwrap()),
        b"aarch64-linux-gnu"
    );
    assert_eq!(
        read_all(&t, at("x86_64-linux-gnu/libc.so.6").unwrap()),
        b"x86_64-linux-gnu"
    );
    assert!(at("other/libc.so.6").is_err());
    assert!(build("*-linux-gnu/libm.so.6").is_err());
}

#[cfg(unix)]
#[test]
fn an_import_into_a_taken_name_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("f");
    std::fs::write(&file, b"x").unwrap();
    let mut t = Tree::new(0, (0, 0));
    import(&mut t, 0, &file, 0).unwrap();
    assert!(import(&mut t, 0, &file, 0).is_err());
    assert_eq!(read_all(&t, t.resolve("f").unwrap()), b"x");
}
