//! `dfdu recover`: items an exited session left staged are listed, then restored or finished;
//! an interrupted delete is listed and dismissed.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use std::fs;
use std::path::Path;
use std::time::Duration;

use common::ok;
use crossbeam_channel::unbounded;
use dfdu_clean::{DeleterMsg, DeleterReq, Identity, Plan, PlanItem};
use dfdu_engine::{DeleteResult, Mode};
use dfdu_tree::{Meta, Tree};

/// A plan item for `p`, with the identity a scan would record: on Windows what `dfdu-sys`
/// reads (volume serial, file id), as the scanners and the deleter's check do.
fn item(tree: &mut Tree, p: &Path) -> PlanItem {
    #[cfg(not(windows))]
    let m = Meta::from_metadata(&fs::symlink_metadata(p).unwrap());
    #[cfg(windows)]
    let m = Meta::from_windows(&dfdu_sys::stat_path(p, false).unwrap());
    let r = tree.new_root(b"/", &m, false);
    PlanItem {
        node: tree.node_ref(r),
        path: p.into(),
        identity: Identity {
            dev: m.dev,
            ino: m.ino,
            kind: m.kind,
            apparent: m.apparent,
            mtime: m.mtime,
        },
        bytes: 0,
        inside: std::sync::Arc::default(),
    }
}

fn recover_status(journals: &Path, args: &[&str]) -> std::process::Output {
    common::bin()
        .arg("recover")
        .arg("--journals")
        .arg(journals)
        .args(args)
        .output()
        .unwrap()
}

fn recover(journals: &Path, args: &[&str]) -> String {
    ok(recover_status(journals, args))
}

/// Runs one plan of `items` in `mode` in a deleter session that then exits.
fn run_plan(journals: &Path, root: &Path, mode: Mode, paths: &[&Path]) {
    let (tx, rx) = unbounded();
    let d = dfdu_clean::spawn_deleter(Some(journals.to_path_buf()), Duration::from_secs(3600), tx);
    let mut tree = Tree::new();
    let items = paths.iter().map(|p| item(&mut tree, p)).collect();
    d.reqs
        .send(DeleterReq::Plan(Plan {
            id: 1,
            mode,
            scan_root: root.into(),
            items,
            protected: None,
        }))
        .unwrap();
    for m in &rx {
        match m {
            DeleterMsg::Item { result, .. } => assert_eq!(result, DeleteResult::Deleted),
            DeleterMsg::Done { .. } => break,
            _ => {}
        }
    }
    // The session ends, and with it the lock on its journal (which on Windows also keeps
    // other handles from reading it).
    drop(d.reqs);
    d.thread.join().unwrap();
}

/// A permanent delete whose end the journal never recorded is listed as interrupted; it can
/// be dismissed, not finished or restored.
#[test]
fn an_interrupted_delete_is_dismissed() {
    let base = dunce::canonicalize(std::env::temp_dir()).unwrap();
    let tmp = tempfile::tempdir_in(base).unwrap();
    let root = tmp.path().join("root");
    fs::create_dir_all(root.join("c")).unwrap();
    fs::write(root.join("c/f"), b"z").unwrap();
    let journals = tmp.path().join("state/ops");
    run_plan(&journals, &root, Mode::Permanent, &[&root.join("c")]);
    // As if the session had been killed before the delete ended: its `done` record is lost.
    let journal = fs::read_dir(&journals)
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    let text = fs::read_to_string(&journal).unwrap();
    assert!(text.contains("\"done\""), "{text}");
    let cut: String = text
        .lines()
        .filter(|l| !l.contains("\"done\""))
        .flat_map(|l| [l, "\n"])
        .collect();
    fs::write(&journal, cut).unwrap();

    let list = recover(&journals, &[]);
    assert!(
        list.contains("  1  ") && list.contains(&root.join("c").display().to_string()),
        "{list}"
    );
    assert!(!list.contains("  2  "), "{list}");
    for refused in ["--finish", "--restore"] {
        for yes in [&[][..], &["--yes"]] {
            let o = recover_status(&journals, &[&[refused, "1"][..], yes].concat());
            assert_eq!(o.status.code(), Some(5), "{refused} {yes:?}");
        }
    }
    assert_eq!(
        recover(&journals, &["--dismiss", "1"]).trim(),
        format!("{}: dismissed", root.join("c").display())
    );
    assert_eq!(recover(&journals, &[]), "nothing unfinished\n");
    assert!(
        fs::read_to_string(&journal)
            .unwrap()
            .contains("dismissed in a later session")
    );
}

#[test]
fn staged_items_are_restored_or_finished() {
    let base = dunce::canonicalize(std::env::temp_dir()).unwrap();
    let tmp = tempfile::tempdir_in(base).unwrap();
    let root = tmp.path().join("root");
    fs::create_dir_all(root.join("a")).unwrap();
    fs::write(root.join("a/f"), b"x").unwrap();
    fs::write(root.join("b"), b"y").unwrap();
    let journals = tmp.path().join("state/ops");
    run_plan(
        &journals,
        &root,
        Mode::Stage,
        &[&root.join("a"), &root.join("b")],
    );
    assert!(!root.join("a").exists() && !root.join("b").exists());

    let list = recover(&journals, &[]);
    assert!(
        list.contains("  1  ")
            && list.contains(&format!("{}: staged at", root.join("a").display())),
        "{list}"
    );
    assert!(
        list.contains("  2  ")
            && list.contains(&format!("{}: staged at", root.join("b").display())),
        "{list}"
    );
    // Without --yes nothing is changed: what would be done is printed.
    let o = recover_status(&journals, &["--restore", "1", "--finish", "2"]);
    let dry = ok(o.clone());
    assert!(dry.contains("a: would be restored"), "{dry}");
    assert!(dry.contains("b: would be deleted"), "{dry}");
    assert!(
        String::from_utf8_lossy(&o.stderr).contains("dry run: nothing was deleted or restored"),
        "{o:?}"
    );
    assert!(!root.join("a").exists() && !root.join("b").exists());
    assert_eq!(recover(&journals, &[]), list);

    let done = recover(&journals, &["--restore", "1", "--finish", "2", "--yes"]);
    assert!(done.contains("a: restored at"), "{done}");
    assert!(done.contains("b: deleted"), "{done}");
    assert_eq!(fs::read(root.join("a/f")).unwrap(), b"x");
    assert!(!root.join("b").exists());
    assert_eq!(recover(&journals, &[]), "nothing unfinished\n");
}

/// A move to the trash whose end the journal lacks is looked for in the trash, found there,
/// and put back. Linux only: on macOS the test would use the user's own Trash.
#[cfg(target_os = "linux")]
#[test]
fn an_interrupted_trash_delete_is_found_in_the_trash() {
    let base = dunce::canonicalize(std::env::temp_dir()).unwrap();
    let tmp = tempfile::tempdir_in(base).unwrap();
    let root = tmp.path().join("root");
    let state = tmp.path().join("state");
    fs::create_dir_all(root.join("web/node_modules/pkg")).unwrap();
    fs::write(root.join("web/package.json"), "{}").unwrap();
    fs::write(root.join("web/node_modules/pkg/index.js"), b"x").unwrap();
    let run = |args: &[&str]| {
        ok(common::bin_in(&state)
            .args(args)
            .env("XDG_DATA_HOME", tmp.path().join("data"))
            .output()
            .unwrap())
    };
    let r = root.to_str().unwrap();
    run(&[
        "clean", r, "--yes", "--all", "--safety", "review", "--mode", "trash",
    ]);
    assert!(!root.join("web/node_modules").exists());
    let journals = state.join("ops");
    let journal = fs::read_dir(&journals)
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    let text = fs::read_to_string(&journal).unwrap();
    let cut: String = text
        .lines()
        .filter(|l| !l.contains("\"done\""))
        .flat_map(|l| [l, "\n"])
        .collect();
    fs::write(&journal, cut).unwrap();

    let list = run(&["recover"]);
    assert!(
        list.contains("node_modules: interrupted (trash); it is in the trash"),
        "{list}"
    );
    let done = run(&["recover", "--restore", "1", "--yes"]);
    assert!(done.contains("restored at"), "{done}");
    assert!(root.join("web/node_modules/pkg/index.js").exists());
    assert_eq!(run(&["recover"]), "nothing unfinished\n");
}
