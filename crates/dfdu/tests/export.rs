//! `dfdu export` and `dfdu import`: an export read back gives the scan's total, plain,
//! compressed, as a snapshot and through stdout and stdin; a broken file is an error.

use std::fs;
use std::io::Write;
use std::process::{Command, Stdio};

fn dfdu(args: &[&std::ffi::OsStr]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_dfdu"))
        .args(args)
        // Scans are recorded in the history: not in the user's.
        .env(
            "DFDU_STATE_DIR",
            std::path::Path::new(env!("CARGO_TARGET_TMPDIR")).join("state"),
        )
        .stdin(Stdio::null())
        .output()
        .unwrap()
}

fn stdout(o: &std::process::Output) -> String {
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    String::from_utf8(o.stdout.clone()).unwrap()
}

#[test]
fn export_then_import() {
    let tmp = tempfile::tempdir().unwrap();
    let tree = tmp.path().join("tree");
    fs::create_dir_all(tree.join("a/b")).unwrap();
    fs::write(tree.join("a/one"), vec![1u8; 20_000]).unwrap();
    fs::write(tree.join("a/b/two"), vec![1u8; 5_000]).unwrap();
    let total = stdout(&dfdu(&[
        "scan".as_ref(),
        tree.as_os_str(),
        "--bytes".as_ref(),
    ]));

    for flag in ["", "--compress", "--snapshot"] {
        let file = tmp.path().join(format!("x{flag}"));
        let mut args = vec![
            "export".as_ref(),
            tree.as_os_str(),
            "-o".as_ref(),
            file.as_os_str(),
        ];
        if !flag.is_empty() {
            args.push(flag.as_ref());
        }
        stdout(&dfdu(&args));
        let bytes = fs::read(&file).unwrap();
        assert_eq!(
            bytes.starts_with(&[0x28, 0xb5, 0x2f, 0xfd]),
            flag == "--compress"
        );
        assert_eq!(bytes.starts_with(b"DFDUSNAP"), flag == "--snapshot");
        let back = stdout(&dfdu(&[
            "import".as_ref(),
            file.as_os_str(),
            "--bytes".as_ref(),
        ]));
        assert_eq!(back, total);
        let summary = stdout(&dfdu(&["import".as_ref(), file.as_os_str()]));
        assert!(summary.contains("entries, written by dfdu"), "{summary}");
    }

    // Through a pipe.
    let out = stdout(&dfdu(&["export".as_ref(), tree.as_os_str()]));
    assert!(out.starts_with("[1,2,{\"progname\":\"dfdu\""), "{out}");
    let mut child = Command::new(env!("CARGO_BIN_EXE_dfdu"))
        .args(["import", "-", "--bytes"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(out.as_bytes())
        .unwrap();
    let o = child.wait_with_output().unwrap();
    assert_eq!(stdout(&o), total);

    let bad = tmp.path().join("bad.json");
    fs::write(&bad, &out[..out.len() / 2]).unwrap();
    let o = dfdu(&["import".as_ref(), bad.as_os_str()]);
    assert_eq!(o.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&o.stderr).contains("bad.json"));
}

/// `export --resume-from` gives what a new scan gives, whether it rescans only what the
/// journal names (macOS) or everything.
#[test]
fn resumed_export_matches_a_new_scan() {
    let tmp = tempfile::tempdir_in(dunce::canonicalize(std::env::temp_dir()).unwrap()).unwrap();
    let tree = tmp.path().join("tree");
    fs::create_dir_all(tree.join("a/b")).unwrap();
    fs::create_dir_all(tree.join("quiet/deep")).unwrap();
    fs::write(tree.join("a/one"), vec![1u8; 20_000]).unwrap();
    fs::write(tree.join("quiet/deep/x"), vec![1u8; 7_000]).unwrap();
    let (old, new) = (tmp.path().join("old.snap"), tmp.path().join("new.snap"));
    let export = |extra: &[&std::ffi::OsStr], out: &std::path::Path| {
        let mut args = vec!["export".as_ref(), tree.as_os_str(), "--snapshot".as_ref()];
        args.extend(["-o".as_ref(), out.as_os_str()]);
        args.extend(extra);
        stdout(&dfdu(&args));
    };
    export(&[], &old);

    fs::remove_file(tree.join("a/one")).unwrap();
    fs::create_dir(tree.join("a/b/c")).unwrap();
    fs::write(tree.join("a/b/c/new"), vec![1u8; 50_000]).unwrap();
    export(&["--resume-from".as_ref(), old.as_os_str()], &new);
    let total = |f: &std::path::Path| {
        stdout(&dfdu(&[
            "import".as_ref(),
            f.as_os_str(),
            "--bytes".as_ref(),
            "--depth".as_ref(),
            "3".as_ref(),
        ]))
    };
    let fresh = tmp.path().join("fresh.snap");
    export(&[], &fresh);
    assert_eq!(total(&new), total(&fresh));
    assert_ne!(total(&new), total(&old));
}
