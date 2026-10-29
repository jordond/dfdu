use std::os::fd::AsFd;

use rustix::fs::CWD;

use super::super::{is_btrfs, open_dir};
use super::*;

/// Keys step through offsets, then types (up to 255), then objectids.
#[test]
fn next_key_carries_into_the_type_then_the_objectid() {
    assert_eq!(next_key((1, 2, 3)), Some((1, 2, 4)));
    assert_eq!(next_key((1, 2, u64::MAX)), Some((1, 3, 0)));
    assert_eq!(next_key((1, 255, u64::MAX)), Some((2, 0, 0)));
    assert_eq!(next_key((u64::MAX, 255, u64::MAX)), None);
    for k in [(0, 0, 0), (7, 84, u64::MAX), (u64::MAX - 1, 255, u64::MAX)] {
        assert!(next_key(k).unwrap() > k, "{k:?}");
    }
}

/// The search ioctl is btrfs's own: elsewhere it fails, and nothing is reported.
#[test]
fn searching_elsewhere_than_btrfs_fails() {
    let d = tempfile::tempdir().unwrap();
    if is_btrfs(d.path()).unwrap() {
        // The btrfs tests cover searches that succeed.
        return;
    }
    let fd = open_dir(CWD, d.path()).unwrap();
    let mut args: Box<SearchArgs> = SearchArgs::boxed();
    let e = tree_search(&mut args, fd.as_fd(), 0, (0, u64::MAX), 1, (0, u64::MAX))
        .err()
        .unwrap();
    assert_eq!(
        e.raw_os_error(),
        Some(rustix::io::Errno::NOTTY.raw_os_error())
    );
}
