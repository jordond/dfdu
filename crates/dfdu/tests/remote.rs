//! `--remote`: `dfdu scan` and `dfdu export` through `dfdu agent`, here run directly instead of
//! through ssh (`DFDU_REMOTE_COMMAND`), give what a local scan gives.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use std::fs;

/// The stdout of `dfdu args`, run locally or with `--remote`.
fn output(args: &[&str], remote: bool) -> String {
    let mut c = common::bin_in(&common::shared_state());
    c.args(args);
    if remote {
        c.args(["--remote", "elsewhere"]).env(
            "DFDU_REMOTE_COMMAND",
            format!("{} agent --threads 2", env!("CARGO_BIN_EXE_dfdu")),
        );
    }
    common::ok(c.output().unwrap())
}

#[test]
fn remote_scans_match_local_ones() {
    let tmp = tempfile::tempdir().unwrap();
    let root = dunce::canonicalize(tmp.path()).unwrap().join("t");
    for i in 0..200 {
        let d = root.join(format!("a{}/b{}", i % 5, i % 3));
        fs::create_dir_all(&d).unwrap();
        fs::write(d.join(format!("f{i}")), vec![7u8; i * 37]).unwrap();
    }
    // Classified on the agent with this side's rules.
    fs::create_dir_all(root.join("web/node_modules/x")).unwrap();
    fs::write(root.join("web/package.json"), b"{}").unwrap();
    fs::write(root.join("web/node_modules/x/a.js"), vec![1u8; 3000]).unwrap();
    let r = root.to_str().unwrap();
    for args in [
        &["scan", r, "--bytes", "--no-history"][..],
        &["scan", r, "--depth", "3", "--top", "50", "--no-history"],
        &[
            "scan",
            r,
            "--format",
            "json",
            "--depth",
            "9",
            "--no-history",
        ],
    ] {
        let local = output(args, false);
        let remote = output(args, true);
        // The table ends with the scan's duration, the JSON document has it as a field.
        let body = |s: &str| {
            s.lines()
                .filter(|l| !l.contains(" items, ") && !l.contains("\"elapsed_ms\""))
                .collect::<Vec<_>>()
                .join("\n")
        };
        assert_eq!(body(&remote), body(&local), "{args:?}");
    }
    assert!(
        output(
            &[
                "scan",
                r,
                "--format",
                "json",
                "--depth",
                "9",
                "--no-history"
            ],
            true
        )
        .contains("\"rule\": \"node.node_modules\"")
    );
    let snap = tmp.path().join("remote.snap");
    output(
        &["export", r, "--snapshot", "-o", snap.to_str().unwrap()],
        true,
    );
    assert_eq!(
        output(&["import", snap.to_str().unwrap(), "--bytes"], false),
        output(&["scan", r, "--bytes", "--no-history"], false)
    );
}
