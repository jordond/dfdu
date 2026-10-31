use dfdu_tree::{ErrKind, Meta};

use super::*;

fn meta(kind: Kind, size: u64, ino: u64) -> Meta {
    Meta {
        kind,
        apparent: size,
        alloc: size,
        mtime: 7,
        dev: 1,
        ino,
        nlink: 1,
        mount: 0,
    }
}

/// The path of `rel` (`/`-separated) below `/r`, joined as the tree joins it on this system.
fn at(rel: &str) -> String {
    let mut p = HostPath::from("/r");
    for c in rel.split('/') {
        p.push(c.as_bytes());
    }
    String::from_utf8(p.as_bytes().to_vec()).unwrap()
}

/// `/r` with the directory `big` (a 300-byte file `f` in it), the files `mid` (100 bytes) and
/// `small` (50 bytes) and the directory `other`, on another filesystem and not entered.
fn tree() -> (Model, Ix) {
    let mut m = Model::default();
    let t = &mut m.tree;
    let root = t.new_root(b"/r", &meta(Kind::Dir, 0, 1), true);
    let big = t.add_dir(root, b"big", &meta(Kind::Dir, 0, 2));
    let f = t.add_file(big, b"f", &meta(Kind::File, 300, 3), false, None);
    let own = t.own(f);
    t.propagate_add(big, &own);
    for (name, size, ino) in [("mid", 100, 4), ("small", 50, 5)] {
        let f = t.add_file(
            root,
            name.as_bytes(),
            &meta(Kind::File, size, ino),
            false,
            None,
        );
        let own = t.own(f);
        t.propagate_add(root, &own);
    }
    let other = t.add_dir(root, b"other", &meta(Kind::Dir, 0, 6));
    t.dir_mut(other).expect("a directory").state = DirState::Skipped;
    (m, root)
}

fn no_note(_: Ix) -> String {
    String::new()
}

fn out(mode: SizeMode, depth: Option<usize>, top: Option<usize>) -> ScanOut<'static> {
    ScanOut {
        mode,
        depth,
        top,
        elapsed_ms: 42,
        note: &no_note,
        outside: None,
    }
}

fn scanned(m: &Model, root: Ix, o: &ScanOut) -> Value {
    let mut w = Vec::new();
    scan(&mut w, m, root, o).unwrap();
    assert_eq!(w.last(), Some(&b'\n'));
    serde_json::from_slice(&w).unwrap()
}

#[test]
fn every_document_starts_with_its_version_the_tool_and_the_root() {
    let doc = document(&HostPath::from("/r"));
    assert_eq!(doc["schema_version"], SCHEMA_VERSION);
    assert_eq!(doc["tool"]["name"], "dfdu");
    assert_eq!(doc["tool"]["version"], env!("CARGO_PKG_VERSION"));
    assert_eq!(doc["root"], "/r");
    assert_eq!(
        [SizeMode::Disk, SizeMode::Apparent, SizeMode::Referenced].map(mode_name),
        ["disk", "apparent", "du"]
    );
}

/// The tree shows `top` children per directory, largest first, and sums the others in `rest`;
/// what was not entered is listed with the reason.
#[test]
fn a_scan_document_lists_the_largest_children_and_sums_the_rest() {
    let (m, root) = tree();
    let doc = scanned(&m, root, &out(SizeMode::Disk, None, Some(2)));
    assert_eq!(doc["size_mode"], "disk");
    assert_eq!(doc["complete"], true);
    assert_eq!(doc["elapsed_ms"], 42);
    assert_eq!(doc["totals"]["disk"], 450);
    let tree = &doc["tree"];
    assert_eq!(
        (&tree["name"], &tree["type"]),
        (&json!("/r"), &json!("dir"))
    );
    let names: Vec<_> = tree["children"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, ["big", "mid"]);
    assert!(
        tree["children"][0].get("children").is_none(),
        "one level by default"
    );
    assert_eq!(tree["rest"]["count"], 2);
    assert_eq!(tree["rest"]["disk"], 50);
    assert_eq!(doc["errors"], json!([]));
    assert_eq!(
        doc["skipped"],
        json!([{ "path": at("other"), "reason": "other-filesystem" }])
    );
    assert!(doc.get("not_entered").is_none());
}

/// An entry that could not be read is listed under `errors`, and the scan is not complete.
#[test]
fn unreadable_entries_are_listed_as_errors() {
    let (mut m, root) = tree();
    let bad = m
        .tree
        .add_file(root, b"bad", &meta(Kind::File, 0, 9), false, None);
    m.tree.set_error(bad, ErrKind::PermissionDenied);
    let doc = scanned(&m, root, &out(SizeMode::Apparent, Some(0), None));
    assert_eq!(doc["complete"], false);
    let errors = doc["errors"].as_array().unwrap();
    assert_eq!(errors.len(), 1);
    assert_eq!(errors[0]["path"], at("bad"));
    assert_eq!(errors[0]["dir"], false);
    assert!(doc["tree"].get("children").is_none(), "depth 0");
}

/// NDJSON is a header, the entries depth first with their paths, the errors and skipped
/// directories, then a summary: one record per line.
#[test]
fn ndjson_is_one_record_per_line_ending_with_the_summary() {
    let (m, root) = tree();
    let mut w = Vec::new();
    scan_ndjson(&mut w, &m, root, &out(SizeMode::Disk, None, None)).unwrap();
    let records: Vec<Value> = std::str::from_utf8(&w)
        .unwrap()
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    let kinds: Vec<_> = records
        .iter()
        .map(|r| r["record"].as_str().unwrap())
        .collect();
    assert_eq!(
        kinds,
        [
            "header", "entry", "entry", "entry", "entry", "entry", "entry", "skipped", "summary"
        ]
    );
    assert_eq!(records[0]["size_mode"], "disk");
    let paths: Vec<_> = records[1..7]
        .iter()
        .map(|r| (r["path"].as_str().unwrap(), r["depth"].as_u64().unwrap()))
        .collect();
    let want = [
        ("/r".to_string(), 0),
        (at("big"), 1),
        (at("big/f"), 2),
        (at("mid"), 1),
        (at("small"), 1),
        (at("other"), 1),
    ];
    assert_eq!(
        paths,
        want.iter()
            .map(|(p, d)| (p.as_str(), *d))
            .collect::<Vec<_>>()
    );
    assert!(records[1].get("name").is_none());
    assert_eq!(records[7]["reason"], "other-filesystem");
    assert_eq!(records[8]["totals"]["disk"], 450);
    assert_eq!(records[8]["complete"], true);
}

/// A matched entry names its rule and category when the model has rules, and its safety level
/// always.
#[test]
fn a_class_names_its_rule_when_the_rules_are_known() {
    let (mut m, _) = tree();
    let env = dfdu_rules::Env::with(dfdu_rules::Platform::Linux, &[("HOME", "/home/u")]);
    let rules = std::sync::Arc::new(dfdu_rules::builtin(&env).0);
    let rule = &rules.rules()[0];
    let c = Class {
        rule: rules.index(&rule.id).unwrap(),
        safety: dfdu_tree::Safety::Review,
        degraded: None,
        flags: dfdu_tree::ClassFlags::NONE,
    };
    assert_eq!(class(&m, c), json!({ "safety": "review" }));
    m.rules = Some(rules.clone());
    assert_eq!(
        class(&m, c),
        json!({ "rule": rule.id, "category": rule.category().id, "safety": "review" })
    );
}
