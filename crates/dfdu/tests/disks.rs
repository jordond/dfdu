//! `dfdu disks` lists the mounted filesystems under a header, one line each.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use common::{dfdu, ok};

#[test]
fn lists_the_filesystems_under_a_header() {
    let out = ok(dfdu(&["disks"]));
    let mut lines = out.lines();
    let header: Vec<&str> = lines.next().unwrap().split_whitespace().collect();
    assert_eq!(
        header,
        ["MOUNTED", "ON", "TYPE", "SIZE", "USED", "FREE", "USE%"]
    );
    let disks: Vec<&str> = lines.collect();
    assert!(!disks.is_empty(), "no filesystem listed:\n{out}");
    #[cfg(unix)]
    assert!(
        disks
            .iter()
            .any(|l| l.split_whitespace().next() == Some("/")),
        "the root filesystem is not listed:\n{out}"
    );
    // The sealed system volume and its data volume: one container's space, shown twice.
    #[cfg(target_os = "macos")]
    if let Some(data) = disks
        .iter()
        .find(|l| l.starts_with("/System/Volumes/Data "))
    {
        assert!(data.contains("  Data  "), "{out}");
        assert!(data.contains("shares space with /"), "{out}");
        let root = disks.iter().find(|l| l.starts_with("/ ")).unwrap();
        assert!(root.contains("System (read-only)"), "{out}");
        assert!(
            root.contains("shares space with /System/Volumes/Data"),
            "{out}"
        );
    }
}
