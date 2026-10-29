use std::os::windows::io::AsHandle;

use super::super::{open_dir, volume_of, wide};
use super::*;

/// A name with a separator or a NUL, or none at all, is refused before anything is opened.
#[test]
fn names_that_are_not_one_component_are_refused() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = open_dir(tmp.path(), false).unwrap();
    let q = LinkQuery::default();
    for bad in [vec![], wide(r"a\b"), wide("a\0b")] {
        let e = q.query(dir.as_handle(), &bad, false).unwrap_err();
        assert_eq!(e.kind(), io::ErrorKind::InvalidInput, "{bad:?}");
    }
}

/// Both ways of asking, by name and through a handle, count every name of a file.
#[test]
fn link_counts_follow_hard_links() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    std::fs::write(root.join("f"), vec![1u8; 5000]).unwrap();
    std::fs::write(root.join("single"), b"x").unwrap();
    let dir = open_dir(root, false).unwrap();
    if !volume_of(dir.as_handle()).unwrap().hard_links {
        return;
    }
    std::fs::hard_link(root.join("f"), root.join("g")).unwrap();
    std::fs::hard_link(root.join("f"), root.join("h")).unwrap();
    // `reparse` takes the handle path; a fresh query tries the by-name call first.
    for reparse in [false, true] {
        let q = LinkQuery::default();
        let f = q.query(dir.as_handle(), &wide("g"), reparse).unwrap();
        assert_eq!((f.nlink, f.size), (3, 5000), "reparse {reparse}");
        let one = q.query(dir.as_handle(), &wide("single"), reparse).unwrap();
        assert_eq!((one.nlink, one.size), (1, 1));
        let e = q
            .query(dir.as_handle(), &wide("missing"), reparse)
            .unwrap_err();
        assert_eq!(e.kind(), io::ErrorKind::NotFound);
    }
}
