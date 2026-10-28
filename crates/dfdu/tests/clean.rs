//! `dfdu clean`: without `--yes` nothing on disk changes; with it the recommended matches are
//! deleted and the others are left with their reason; the JSON output validates against
//! `schema/clean.v1.json`.

use std::fs;
use std::path::Path;
use std::process::{Command, Output, Stdio};
use std::time::{Duration, SystemTime};

use serde_json::Value;

fn dfdu(state: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_dfdu"))
        .args(args)
        .env("DFDU_STATE_DIR", state)
        .stdin(Stdio::null())
        .output()
        .unwrap()
}

/// Sets the modification time of `p` (a file or directory) `days` back.
fn age(p: &Path, days: u64) {
    let t = SystemTime::now() - Duration::from_secs(days * 86_400);
    fs::File::open(p).unwrap().set_modified(t).unwrap();
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
    const BASE: &str = "https://github.com/jordond/dfdu/schema/";
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../schema");
    let mut c = boon::Compiler::new();
    for s in ["scan.v1.json", "report.v1.json", "clean.v1.json"] {
        let v: Value = serde_json::from_slice(&fs::read(dir.join(s)).unwrap()).unwrap();
        c.add_resource(&format!("{BASE}{s}"), v).unwrap();
    }
    let mut schemas = boon::Schemas::new();
    let ix = c
        .compile(&format!("{BASE}clean.v1.json"), &mut schemas)
        .unwrap();
    (schemas, ix)
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
        let o = dfdu(&state, &args);
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
    let o = dfdu(&state, &["clean", r]);
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
        let o = dfdu(
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
            .find(|i| i["path"].as_str().unwrap().contains("/new/"))
            .unwrap();
        assert!(
            new["reason"].as_str().unwrap().contains("not recommended"),
            "{new}"
        );
    }

    // Review-level matches only at --safety review; staged, then committed before exit.
    let o = dfdu(
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
        String::from_utf8_lossy(&dfdu(&state, &["recover"]).stdout).trim(),
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
    // Stage the old project's dependencies, then lose the commit: as if the session had been
    // killed inside its undo window.
    let o = dfdu(&state, &["clean", r, "--yes", "--mode", "stage"]);
    assert!(o.status.success());
    let journal = fs::read_dir(state.join("ops"))
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    let text = fs::read_to_string(&journal).unwrap();
    let cut: String = text
        .lines()
        .filter(|l| !l.contains("\"commit-"))
        .map(|l| format!("{l}\n"))
        .collect();
    fs::write(&journal, cut).unwrap();
    // The staged copy is gone already (the commit ran); put a stand-in there.
    let list = String::from_utf8(dfdu(&state, &["recover"]).stdout).unwrap();
    let stage = list
        .split(": staged at ")
        .nth(1)
        .map(|s| std::path::PathBuf::from(s.trim()))
        .unwrap_or_else(|| panic!("{list}"));
    fs::create_dir_all(stage.join("pkg")).unwrap();
    fs::write(stage.join("pkg/index.js"), b"x").unwrap();

    let o = dfdu(&state, &["clean", r, "--finish-staged"]);
    let err = String::from_utf8_lossy(&o.stderr);
    assert!(
        err.contains("1 item earlier sessions left unfinished"),
        "{err}"
    );
    assert!(stage.exists(), "without --yes nothing is done");
    let o = dfdu(&state, &["clean", r, "--finish-staged", "--yes"]);
    let err = String::from_utf8_lossy(&o.stderr);
    assert!(err.contains("staged earlier, deleted"), "{err}");
    assert!(!stage.exists());
    assert_eq!(
        String::from_utf8_lossy(&dfdu(&state, &["recover"]).stdout).trim(),
        "nothing unfinished"
    );
}
