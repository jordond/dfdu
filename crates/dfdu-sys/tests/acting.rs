//! A root process started by sudo acts for the user who ran it (`dfdu_sys::acting`). Runs only
//! as root with `SUDO_UID` set to another user (CI's root btrfs step runs tests through
//! `sudo`); skipped (passing) otherwise.

#![cfg(unix)]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::os::unix::fs::MetadataExt;
use std::process::Command;

const INNER: &str = "DFDU_ACTING_INNER";

fn sudo_uid() -> Option<u32> {
    if !rustix::process::geteuid().is_root() {
        return None;
    }
    std::env::var("SUDO_UID")
        .ok()?
        .parse()
        .ok()
        .filter(|&u| u != 0)
}

/// `adopt` executes the process again when `HOME` is not the user's, so the checks run in a
/// child of this test binary started with root's `HOME`.
#[test]
fn root_acts_for_the_sudo_user() {
    if sudo_uid().is_none() || std::env::var_os(INNER).is_some() {
        return;
    }
    let out = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "inner", "--nocapture", "--test-threads=1"])
        .env(INNER, "1")
        .env("HOME", "/root")
        .output()
        .unwrap();
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success() && text.contains("inner checked"),
        "{text}{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn inner() {
    let Some(uid) = sudo_uid() else { return };
    if std::env::var_os(INNER).is_none() {
        return;
    }
    let dfdu_sys::acting::Adopted::User(u) = dfdu_sys::acting::adopt().unwrap() else {
        panic!("not acting for anyone");
    };
    assert_eq!(u.uid, uid);
    assert_eq!(
        std::env::var_os("HOME").as_deref(),
        Some(u.home.as_os_str())
    );
    assert_eq!(dfdu_sys::acting::owner_uid(), uid);

    // Commands run as the user, with their groups and home.
    let run = |args: &[&str]| {
        let mut c = Command::new(args[0]);
        c.args(&args[1..]);
        dfdu_sys::acting::run_as(&mut c);
        let o = c.output().unwrap();
        assert!(o.status.success(), "{args:?}");
        String::from_utf8(o.stdout).unwrap().trim().to_owned()
    };
    assert_eq!(run(&["id", "-u"]), uid.to_string());
    assert_eq!(run(&["id", "-g"]), u.gid.to_string());
    let mut groups: Vec<u32> = run(&["id", "-G"])
        .split_whitespace()
        .map(|g| g.parse().unwrap())
        .collect();
    groups.sort_unstable();
    let mut want = u.groups.clone();
    want.sort_unstable();
    want.dedup();
    groups.dedup();
    assert_eq!(groups, want);
    assert_eq!(
        run(&["sh", "-c", "echo $HOME"]),
        u.home.display().to_string()
    );

    // What root makes below the home goes back to the user; a symlink's target and what is
    // outside the home do not.
    let top = u.home.join(format!(".dfdu-acting-{}", std::process::id()));
    let deep = top.join("a/b");
    std::fs::create_dir_all(&deep).unwrap();
    std::fs::write(deep.join("f"), b"x").unwrap();
    let outside = tempfile::NamedTempFile::new().unwrap();
    std::os::unix::fs::symlink(outside.path(), deep.join("link")).unwrap();
    let outside_dir = tempfile::tempdir().unwrap();
    std::fs::write(outside_dir.path().join("g"), b"y").unwrap();
    std::os::unix::fs::symlink(outside_dir.path(), top.join("a/dirlink")).unwrap();
    // A root-owned file linked in from elsewhere (the user could do that) keeps its owner.
    let linked_from = u
        .home
        .join(format!(".dfdu-acting-root-{}", std::process::id()));
    std::fs::write(&linked_from, b"root's").unwrap();
    std::fs::hard_link(&linked_from, deep.join("hard")).unwrap();
    dfdu_sys::acting::give_back(&top.join("a"), true);
    for p in [top.clone(), top.join("a"), deep.clone(), deep.join("f")] {
        assert_eq!(
            std::fs::symlink_metadata(&p).unwrap().uid(),
            uid,
            "{}",
            p.display()
        );
    }
    for p in [
        outside.path().to_path_buf(),
        outside_dir.path().to_path_buf(),
        outside_dir.path().join("g"),
        linked_from.clone(),
    ] {
        assert_eq!(std::fs::metadata(&p).unwrap().uid(), 0, "{}", p.display());
    }
    // `..` is refused.
    dfdu_sys::acting::give_back(&top.join("a/../a"), true);
    std::fs::remove_file(&linked_from).unwrap();
    let other = tempfile::tempdir().unwrap();
    dfdu_sys::acting::give_back(other.path(), true);
    assert_eq!(std::fs::metadata(other.path()).unwrap().uid(), 0);
    std::fs::remove_dir_all(&top).unwrap();
    println!("inner checked");
}
