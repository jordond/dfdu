//! `dfdu scan --type` and `--by-type`: sizes count only the matching files, entries without
//! any are left out, and the breakdown adds up.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use std::fs;

use common::ok;

/// Runs `dfdu args --no-history`.
fn unrecorded(args: &[&std::ffi::OsStr]) -> std::process::Output {
    common::bin()
        .args(args)
        .arg("--no-history")
        .output()
        .unwrap()
}

#[test]
fn type_filter_and_breakdown() {
    let tmp = tempfile::tempdir().unwrap();
    let tree = tmp.path().join("tree");
    fs::create_dir_all(tree.join("films/old")).unwrap();
    fs::create_dir_all(tree.join("music")).unwrap();
    fs::write(tree.join("films/a.mp4"), vec![1u8; 40_000]).unwrap();
    fs::write(tree.join("films/old/b.MKV"), vec![1u8; 20_000]).unwrap();
    fs::write(tree.join("films/notes.txt"), vec![1u8; 4_000]).unwrap();
    fs::write(tree.join("music/c.flac"), vec![1u8; 30_000]).unwrap();
    let bytes = |types: &str| {
        let o = unrecorded(&[
            "scan".as_ref(),
            tree.as_os_str(),
            "--apparent".as_ref(),
            "--bytes".as_ref(),
            "--type".as_ref(),
            types.as_ref(),
        ]);
        ok(o).trim().parse::<u64>().unwrap()
    };
    assert_eq!(bytes("video"), 60_000);
    assert_eq!(bytes("videos,audio"), 90_000);
    assert_eq!(bytes(".flac,txt"), 34_000);

    let out = ok(unrecorded(&[
        "scan".as_ref(),
        tree.as_os_str(),
        "--type".as_ref(),
        "video".as_ref(),
        "--depth".as_ref(),
        "3".as_ref(),
    ]));
    assert!(out.starts_with("only video: 2 files\n"), "{out}");
    assert!(out.contains("films/") && out.contains("b.MKV"), "{out}");
    assert!(
        !out.contains("music") && !out.contains("notes.txt"),
        "{out}"
    );

    let out = ok(unrecorded(&[
        "scan".as_ref(),
        tree.as_os_str(),
        "--apparent".as_ref(),
        "--by-type".as_ref(),
    ]));
    let by_type = out.split("By type:\n").nth(1).expect(&out);
    let lines: Vec<&str> = by_type.lines().take(3).collect();
    assert!(
        lines[0].contains("video") && lines[0].contains("2 files"),
        "{out}"
    );
    assert!(
        lines[1].contains("audio") && lines[1].contains("1 file"),
        "{out}"
    );
    assert!(lines[2].contains("document"), "{out}");
    assert!(out.contains(".mkv") && out.contains(".flac"), "{out}");

    let o = unrecorded(&[
        "scan".as_ref(),
        tree.as_os_str(),
        "--type".as_ref(),
        "video".as_ref(),
        "--format".as_ref(),
        "json".as_ref(),
    ]);
    assert_eq!(o.status.code(), Some(2));
    let o = unrecorded(&[
        "scan".as_ref(),
        tree.as_os_str(),
        "--type".as_ref(),
        "a/b".as_ref(),
    ]);
    assert_eq!(o.status.code(), Some(2));
}
