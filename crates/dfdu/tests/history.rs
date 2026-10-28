//! `dfdu scan` records each scan; `dfdu history` lists them; `dfdu diff` compares a directory
//! with its last recorded scan or the one closest to `--since` ago, or two saved scans, and
//! its JSON validates against `schema/diff.v1.json`.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use std::fs;
use std::path::Path;

use common::{dfdu_in, ok};
use serde_json::Value;

fn schema() -> (boon::Schemas, boon::SchemaIndex) {
    let (schemas, ix) = common::schemas(&["scan.v1.json", "diff.v1.json"], &["diff.v1.json"]);
    (schemas, ix[0])
}

/// (path relative to the root, status, change in disk usage) of each change listed.
fn changes(doc: &Value, root: &Path) -> Vec<(String, String, i64)> {
    let mut v: Vec<_> = doc["changes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| {
            let p = Path::new(c["path"].as_str().unwrap());
            (
                p.strip_prefix(root).unwrap().display().to_string(),
                c["status"].as_str().unwrap().to_owned(),
                c["delta_disk"].as_i64().unwrap(),
            )
        })
        .collect();
    v.sort();
    v
}

#[test]
fn diff_against_history_and_files() {
    let tmp = tempfile::tempdir().unwrap();
    let base = dunce::canonicalize(tmp.path()).unwrap();
    let root = base.join("tree");
    let state = base.join("state");
    for d in ["grows", "goes", "stays"] {
        fs::create_dir_all(root.join(d)).unwrap();
        fs::write(root.join(d).join("f"), vec![1u8; 100_000]).unwrap();
    }
    let r = root.to_str().unwrap();
    assert_eq!(
        ok(dfdu_in(&state, &["history"])).trim(),
        "no scans recorded"
    );
    let o = dfdu_in(&state, &["diff", r]);
    assert_eq!(o.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&o.stderr).contains("no earlier scan"));
    let snap1 = base.join("one.snap");
    ok(dfdu_in(
        &state,
        &["export", r, "--snapshot", "-o", snap1.to_str().unwrap()],
    ));
    ok(dfdu_in(&state, &["scan", r]));
    let list = ok(dfdu_in(&state, &["history", r]));
    // The diff above recorded its scan too.
    assert_eq!(list.lines().count(), 2, "{list}");
    assert!(list.contains(r), "{list}");

    fs::write(root.join("grows/g"), vec![2u8; 3 << 20]).unwrap();
    fs::remove_dir_all(root.join("goes")).unwrap();
    fs::create_dir(root.join("new")).unwrap();
    fs::write(root.join("new/n"), vec![3u8; 2 << 20]).unwrap();
    let snap2 = base.join("two.snap");
    ok(dfdu_in(
        &state,
        &["export", r, "--snapshot", "-o", snap2.to_str().unwrap()],
    ));

    let (schemas, ix) = schema();
    let json = |args: &[&str]| -> Value {
        let mut a = args.to_vec();
        a.extend(["--format", "json", "--min-change", "0"]);
        let doc: Value = serde_json::from_str(&ok(dfdu_in(&state, &a))).unwrap();
        if let Err(e) = schemas.validate(&doc, ix) {
            panic!("{e:#}\n{doc:#}");
        }
        doc
    };
    let doc = json(&["diff", r]);
    assert_eq!(doc["old"]["source"], "history");
    assert_eq!(doc["new"]["source"], "scan");
    let got = changes(&doc, &root);
    let names: Vec<(&str, &str)> = got
        .iter()
        .map(|(p, s, _)| (p.as_str(), s.as_str()))
        .collect();
    assert_eq!(
        names,
        [("goes", "gone"), ("grows", "grown"), ("new", "new")]
    );
    assert!(got[1].2 >= 3 << 20 && got[2].2 >= 2 << 20, "{got:?}");
    assert!(doc["total"]["delta_disk"].as_i64().unwrap() >= (5 << 20) - 200_000);

    // Since the diff recorded the tree as it is now, the latest scan shows nothing new, and
    // the scan closest to a week ago (the first) shows it all again.
    assert_eq!(changes(&json(&["diff", r]), &root), []);
    assert_eq!(
        changes(&json(&["diff", r, "--since", "7d"]), &root).len(),
        3
    );
    let table = ok(dfdu_in(
        &state,
        &["diff", r, "--since", "7d", "--min-change", "0"],
    ));
    assert!(
        table.contains("(new)") && table.contains("(gone)"),
        "{table}"
    );

    // Two files (directories are compared, as in the history).
    let doc = json(&["diff", snap1.to_str().unwrap(), snap2.to_str().unwrap()]);
    assert_eq!(doc["old"]["source"], snap1.to_str().unwrap());
    let names: Vec<String> = changes(&doc, &root).into_iter().map(|c| c.0).collect();
    assert_eq!(names, ["goes", "grows", "new"].map(String::from));
    // Scans with other options are not compared.
    let o = dfdu_in(&state, &["diff", r, "--cross-mounts"]);
    assert_eq!(o.status.code(), Some(1));
}

/// Piped `dfdu PATH --no-history` scans without recording, like `dfdu scan PATH --no-history`.
#[test]
fn piped_browse_honours_no_history() {
    let tmp = tempfile::tempdir().unwrap();
    let base = dunce::canonicalize(tmp.path()).unwrap();
    let root = base.join("tree");
    let state = base.join("state");
    fs::create_dir_all(&root).unwrap();
    fs::write(root.join("f"), vec![1u8; 10_000]).unwrap();
    let r = root.to_str().unwrap();
    ok(dfdu_in(&state, &[r, "--no-history"]));
    assert_eq!(
        ok(dfdu_in(&state, &["history"])).trim(),
        "no scans recorded"
    );
    ok(dfdu_in(&state, &[r]));
    assert_eq!(ok(dfdu_in(&state, &["history", r])).lines().count(), 1);
}
