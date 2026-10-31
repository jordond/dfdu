use std::fs;
use std::io::{self, Write};
use std::path::Path;

use super::*;

/// The names in `dir`, sorted.
fn names(dir: &Path) -> Vec<String> {
    let mut v: Vec<String> = fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    v.sort();
    v
}

#[test]
fn an_atomic_write_replaces_the_file_and_leaves_no_temporary() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("snap");
    fs::write(&path, b"old").unwrap();
    let n = write_atomic(&path, |f| f.write_all(b"new").map(|()| 3)).unwrap();
    assert_eq!(n, 3);
    assert_eq!(fs::read(&path).unwrap(), b"new");
    assert_eq!(names(tmp.path()), ["snap"]);
}

#[test]
fn a_failed_atomic_write_keeps_the_old_file() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("snap");
    fs::write(&path, b"old").unwrap();
    let e = write_atomic(&path, |f| {
        f.write_all(b"half")?;
        Err::<(), _>(io::Error::other("stopped"))
    })
    .unwrap_err();
    assert_eq!(e.to_string(), "stopped");
    assert_eq!(fs::read(&path).unwrap(), b"old");
    assert_eq!(names(tmp.path()), ["snap"]);
}

#[test]
fn an_atomic_write_removes_temporaries_of_dead_writers_only() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("snap");
    // A writer that died before renaming, and files that only look like its temporary.
    fs::write(tmp.path().join(".snap.12345.tmp"), b"stale").unwrap();
    fs::write(tmp.path().join(".snap.x1.tmp"), b"other").unwrap();
    fs::write(tmp.path().join(".other.12345.tmp"), b"other").unwrap();
    write_atomic(&path, |f| f.write_all(b"new")).unwrap();
    assert_eq!(
        names(tmp.path()),
        [".other.12345.tmp", ".snap.x1.tmp", "snap"]
    );
}

#[test]
fn an_atomic_write_needs_a_file_name() {
    let e = write_atomic(Path::new("/"), |_| Ok(())).unwrap_err();
    assert_eq!(e.kind(), io::ErrorKind::InvalidInput);
}

#[test]
fn open_refuses_what_is_in_no_format() {
    let limits = Limits::default();
    assert!(matches!(
        open(&b"not a scan"[..], &limits),
        Err(ImportError::Format { .. })
    ));
}
