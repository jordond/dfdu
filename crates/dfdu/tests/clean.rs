//! `dfdu clean`: without `--yes` nothing on disk changes; with it the recommended matches are
//! deleted and the others are left with their reason; the JSON output validates against
//! `schema/clean.v1.json`.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use std::fs;
use std::path::Path;
use std::time::{Duration, SystemTime};

use common::dfdu_in;
use serde_json::Value;

/// Sets the modification time of `p` (a file or directory) `days` back.
fn age(p: &Path, days: u64) {
    let t = SystemTime::now() - Duration::from_secs(days * 86_400);
    let mut o = fs::OpenOptions::new();
    o.read(true);
    // Windows opens a directory only with FILE_FLAG_BACKUP_SEMANTICS, and setting its times
    // needs FILE_WRITE_ATTRIBUTES.
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        o.access_mode(0x0100).custom_flags(0x0200_0000);
    }
    o.open(p).unwrap().set_modified(t).unwrap();
}

/// Every path below `root` with its size, sorted.
fn listing(root: &Path) -> Vec<(String, u64)> {
    let mut out = vec![];
    let mut stack = vec![root.to_path_buf()];
    while let Some(d) = stack.pop() {
        for e in fs::read_dir(&d).unwrap() {
            let e = e.unwrap();
            let md = fs::symlink_metadata(e.path()).unwrap();
            out.push((e.path().display().to_string(), md.len()));
            if md.is_dir() {
                stack.push(e.path());
            }
        }
    }
    out.sort();
    out
}

/// An old project's big dependencies (recommended), a fresh one's (not: changed this week), and
/// a project whose dependencies have no lockfile (review: not deleted at the default level).
fn fixture(root: &Path) {
    for (p, days) in [("old", 60), ("new", 0), ("nolock", 60)] {
        let nm = root.join(p).join("node_modules/pkg");
        fs::create_dir_all(&nm).unwrap();
        fs::write(root.join(p).join("package.json"), "{}").unwrap();
        if p != "nolock" {
            fs::write(root.join(p).join("package-lock.json"), "{}").unwrap();
        }
        // The rule recommends dependencies of 50 MiB or more.
        let size = if p == "old" { 51 << 20 } else { 30_000 };
        fs::write(nm.join("index.js"), vec![3u8; size]).unwrap();
        age(&nm.join("index.js"), days);
        age(&nm, days);
        age(nm.parent().unwrap(), days);
    }
}

fn schema() -> (boon::Schemas, boon::SchemaIndex) {
    let (schemas, ix) = common::schemas(
        &["scan.v1.json", "report.v1.json", "clean.v1.json"],
        &["clean.v1.json"],
    );
    (schemas, ix[0])
}

fn outcomes(doc: &Value) -> Vec<(String, String)> {
    let mut v: Vec<(String, String)> = doc["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| {
            let p = i["path"].as_str().unwrap();
            let project = Path::new(p).parent().unwrap().file_name().unwrap();
            (
                project.to_str().unwrap().to_owned(),
                i["outcome"].as_str().unwrap().to_owned(),
            )
        })
        .collect();
    v.sort();
    v
}

#[test]
fn clean_without_yes_changes_nothing() {
    let tmp = tempfile::tempdir().unwrap();
    let root = dunce::canonicalize(tmp.path()).unwrap().join("tree");
    let state = tmp.path().join("state");
    fs::create_dir(&root).unwrap();
    fixture(&root);
    let before = listing(&root);
    let (schemas, ix) = schema();
    let r = root.to_str().unwrap();
    for extra in [
        &[][..],
        &["--all"],
        &["--mode", "delete", "--all", "--safety", "review"],
        &["--mode", "stage", "--allow-commands"],
    ] {
        let mut args = vec!["clean", r, "--format", "json"];
        args.extend(extra);
        let o = dfdu_in(&state, &args);
        assert_eq!(
            o.status.code(),
            Some(0),
            "{}",
            String::from_utf8_lossy(&o.stderr)
        );
        let doc: Value = serde_json::from_slice(&o.stdout).unwrap();
        if let Err(e) = schemas.validate(&doc, ix) {
            panic!("{e:#}\n{doc:#}");
        }
        assert_eq!(doc["dry_run"], true);
        assert_eq!(doc["summary"]["deleted"]["items"], 0);
        assert!(
            doc["summary"]["planned"]["items"].as_u64().unwrap() >= 1,
            "{extra:?}"
        );
        assert_eq!(listing(&root), before, "{extra:?}");
    }
    let o = dfdu_in(&state, &["clean", r]);
    let text = String::from_utf8_lossy(&o.stdout);
    assert!(text.contains("dry run: 1 item"), "{text}");
    assert_eq!(listing(&root), before);
}

#[test]
fn clean_deletes_what_is_recommended() {
    let tmp = tempfile::tempdir().unwrap();
    let root = dunce::canonicalize(tmp.path()).unwrap().join("tree");
    let state = tmp.path().join("state");
    fs::create_dir(&root).unwrap();
    fixture(&root);
    let (schemas, ix) = schema();
    let r = root.to_str().unwrap();
    for mode in ["delete", "stage"] {
        let o = dfdu_in(
            &state,
            &["clean", r, "--yes", "--mode", mode, "--format", "json"],
        );
        assert_eq!(
            o.status.code(),
            Some(0),
            "{}",
            String::from_utf8_lossy(&o.stderr)
        );
        let doc: Value = serde_json::from_slice(&o.stdout).unwrap();
        if let Err(e) = schemas.validate(&doc, ix) {
            panic!("{e:#}\n{doc:#}");
        }
        let want: Vec<(String, String)> = if mode == "delete" {
            vec![
                ("new".into(), "skipped".into()),
                ("nolock".into(), "skipped".into()),
                ("old".into(), "deleted".into()),
            ]
        } else {
            vec![
                ("new".into(), "skipped".into()),
                ("nolock".into(), "skipped".into()),
            ]
        };
        assert_eq!(outcomes(&doc), want, "{mode}");
        assert!(!root.join("old/node_modules").exists());
        assert!(root.join("old/package.json").exists());
        assert!(root.join("new/node_modules/pkg/index.js").exists());
        assert!(root.join("nolock/node_modules/pkg/index.js").exists());
        let new = doc["items"]
            .as_array()
            .unwrap()
            .iter()
            .find(|i| Path::new(i["path"].as_str().unwrap()).starts_with(root.join("new")))
            .unwrap();
        assert!(
            new["reason"].as_str().unwrap().contains("not recommended"),
            "{new}"
        );
    }

    // Review-level matches only at --safety review; staged, then committed before exit.
    let o = dfdu_in(
        &state,
        &[
            "clean", r, "--yes", "--all", "--safety", "review", "--mode", "stage",
        ],
    );
    assert_eq!(
        o.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&o.stderr)
    );
    let text = String::from_utf8_lossy(&o.stdout);
    assert!(text.contains("2 items"), "{text}");
    assert!(!root.join("nolock/node_modules").exists());
    assert!(!root.join("new/node_modules").exists());
    let staged: Vec<_> = walk_files(&state.join("stage"));
    assert!(staged.is_empty(), "left in the state directory: {staged:?}");
    assert_eq!(
        String::from_utf8_lossy(&dfdu_in(&state, &["recover"]).stdout).trim(),
        "nothing unfinished"
    );
}

/// Files below `dir`.
fn walk_files(dir: &Path) -> Vec<std::path::PathBuf> {
    let mut out = vec![];
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        let Ok(rd) = fs::read_dir(&d) else { continue };
        for e in rd.flatten() {
            if e.path().is_dir() {
                stack.push(e.path());
            } else {
                out.push(e.path());
            }
        }
    }
    out
}

/// Stages directory `item` of `scan_root` with a deleter journaling into `ops`, which then
/// ends before the commit: what a session killed inside its undo window leaves.
fn stage_without_commit(ops: &Path, scan_root: &Path, item: &Path) {
    #[cfg(not(windows))]
    let m = dfdu_tree::Meta::from_metadata(&fs::symlink_metadata(item).unwrap());
    #[cfg(windows)]
    let m = dfdu_tree::Meta::from_windows(&dfdu_sys::stat_path(item, false).unwrap());
    let mut tree = dfdu_tree::Tree::new();
    let node = tree.new_root(b"/", &m, false);
    let plan = dfdu_clean::Plan {
        id: 1,
        mode: dfdu_clean::Mode::Stage,
        scan_root: scan_root.into(),
        items: vec![dfdu_clean::PlanItem {
            node: tree.node_ref(node),
            path: item.into(),
            identity: dfdu_clean::Identity {
                dev: m.dev,
                ino: m.ino,
                kind: m.kind,
                apparent: m.apparent,
                mtime: m.mtime,
            },
            bytes: 0,
            inside: std::sync::Arc::default(),
        }],
        protected: None,
    };
    let (tx, rx) = crossbeam_channel::unbounded();
    let d = dfdu_clean::spawn_deleter(Some(ops.into()), Duration::from_secs(3600), tx);
    d.reqs.send(dfdu_clean::DeleterReq::Plan(plan)).unwrap();
    for m in &rx {
        if let dfdu_clean::DeleterMsg::Item { result, .. } = &m {
            assert_eq!(*result, dfdu_clean::DeleteResult::Deleted);
        }
        if matches!(m, dfdu_clean::DeleterMsg::Done { .. }) {
            break;
        }
    }
    drop(d.reqs);
    d.thread.join().unwrap();
}

/// What an earlier session left staged is reported by `clean`, and finished with
/// `--finish-staged --yes`; without `--yes` nothing is done.
#[test]
fn clean_reports_and_finishes_what_earlier_sessions_staged() {
    let tmp = tempfile::tempdir().unwrap();
    let root = dunce::canonicalize(tmp.path()).unwrap().join("tree");
    let state = tmp.path().join("state");
    fs::create_dir(&root).unwrap();
    fixture(&root);
    let r = root.to_str().unwrap();
    stage_without_commit(&state.join("ops"), &root, &root.join("old/node_modules"));
    let list = String::from_utf8(dfdu_in(&state, &["recover"]).stdout).unwrap();
    let staged = list
        .split(": staged at ")
        .nth(1)
        .map_or_else(|| panic!("{list}"), |s| std::path::PathBuf::from(s.trim()));
    assert!(staged.join("pkg/index.js").exists(), "{list}");

    let o = dfdu_in(&state, &["clean", r, "--finish-staged"]);
    let err = String::from_utf8_lossy(&o.stderr);
    assert!(
        err.contains("1 item earlier sessions left unfinished"),
        "{err}"
    );
    assert!(staged.exists(), "without --yes nothing is done");
    let o = dfdu_in(&state, &["clean", r, "--finish-staged", "--yes"]);
    let err = String::from_utf8_lossy(&o.stderr);
    assert!(err.contains("staged earlier, deleted"), "{err}");
    assert!(!staged.exists());
    assert_eq!(
        String::from_utf8_lossy(&dfdu_in(&state, &["recover"]).stdout).trim(),
        "nothing unfinished"
    );
}
