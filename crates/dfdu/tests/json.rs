//! `dfdu scan --format json|ndjson` and `dfdu report --format json`: the output validates
//! against the schemas in `schema/`, its totals are the scan's, names that are not UTF-8 come
//! back from their base64, and `--fail-above` sets the exit code.

use std::fs;
use std::path::Path;
use std::process::{Command, Output, Stdio};

use serde_json::Value;

fn dfdu(args: &[&std::ffi::OsStr]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_dfdu"))
        .args(args)
        .stdin(Stdio::null())
        .output()
        .unwrap()
}

fn json(o: &Output) -> Value {
    serde_json::from_slice(&o.stdout).unwrap_or_else(|e| {
        panic!(
            "{e}: {}\n{}",
            String::from_utf8_lossy(&o.stdout),
            String::from_utf8_lossy(&o.stderr)
        )
    })
}

const SCHEMAS: [&str; 3] = ["scan.v1.json", "scan-ndjson.v1.json", "report.v1.json"];
const BASE: &str = "https://github.com/jordond/dfdu/schema/";

/// The published schemas, compiled.
fn schemas() -> (boon::Schemas, Vec<boon::SchemaIndex>) {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../schema");
    let mut c = boon::Compiler::new();
    for s in SCHEMAS {
        let v: Value = serde_json::from_slice(&fs::read(dir.join(s)).unwrap()).unwrap();
        assert_eq!(v["$id"], format!("{BASE}{s}"));
        c.add_resource(&format!("{BASE}{s}"), v).unwrap();
    }
    let mut schemas = boon::Schemas::new();
    let ix = SCHEMAS
        .iter()
        .map(|s| c.compile(&format!("{BASE}{s}"), &mut schemas).unwrap())
        .collect();
    (schemas, ix)
}

fn valid(schemas: &boon::Schemas, ix: boon::SchemaIndex, v: &Value) {
    if let Err(e) = schemas.validate(v, ix) {
        panic!("{e:#}\n{}", serde_json::to_string_pretty(v).unwrap());
    }
}

/// A tree with a project's dependencies and build output, an old cache, and on Unix a name
/// that is not UTF-8.
fn fixture(root: &Path) {
    fs::create_dir_all(root.join("web/node_modules/left-pad")).unwrap();
    fs::write(root.join("web/package.json"), "{}").unwrap();
    fs::write(root.join("web/package-lock.json"), "{}").unwrap();
    fs::write(
        root.join("web/node_modules/left-pad/index.js"),
        vec![7u8; 40_000],
    )
    .unwrap();
    fs::create_dir_all(root.join("docs")).unwrap();
    fs::write(root.join("docs/notes.txt"), vec![1u8; 9_000]).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        let name = std::ffi::OsStr::from_bytes(b"caf\xe9.txt");
        if let Err(e) = fs::write(root.join("docs").join(name), b"x") {
            // macOS APFS refuses names that are not UTF-8.
            assert_eq!(e.raw_os_error(), Some(libc_eilseq()), "{e}");
        }
    }
}

#[cfg(unix)]
fn libc_eilseq() -> i32 {
    if cfg!(target_os = "macos") { 92 } else { 84 }
}

fn decode_b64(s: &str) -> Vec<u8> {
    let val = |c: u8| match c {
        b'A'..=b'Z' => c - b'A',
        b'a'..=b'z' => c - b'a' + 26,
        b'0'..=b'9' => c - b'0' + 52,
        b'+' => 62,
        _ => 63,
    };
    let mut out = vec![];
    for q in s.trim_end_matches('=').as_bytes().chunks(4) {
        let n = q
            .iter()
            .enumerate()
            .fold(0u32, |n, (i, &c)| n | (val(c) as u32) << (18 - 6 * i));
        out.extend(&n.to_be_bytes()[1..q.len()]);
    }
    out
}

#[test]
fn scan_json_validates_and_adds_up() {
    let tmp = tempfile::tempdir().unwrap();
    let root = dunce::canonicalize(tmp.path()).unwrap();
    fixture(&root);
    let (schemas, ix) = schemas();
    let bytes = dfdu(&["scan".as_ref(), root.as_os_str(), "--bytes".as_ref()]);
    let total: u64 = String::from_utf8(bytes.stdout)
        .unwrap()
        .trim()
        .parse()
        .unwrap();

    let o = dfdu(&[
        "scan".as_ref(),
        root.as_os_str(),
        "--format".as_ref(),
        "json".as_ref(),
        "--depth".as_ref(),
        "3".as_ref(),
        "--top".as_ref(),
        "1".as_ref(),
    ]);
    assert!(o.status.success());
    let doc = json(&o);
    valid(&schemas, ix[0], &doc);
    let mut bad = doc.clone();
    bad["tree"]["children"][0]["type"] = "folder".into();
    assert!(
        schemas.validate(&bad, ix[0]).is_err(),
        "the schema checks entries"
    );
    assert_eq!(doc["root"], root.to_str().unwrap());
    assert_eq!(doc["totals"]["disk"], total);
    assert_eq!(doc["tree"]["disk"], total);
    assert_eq!(doc["complete"], true);
    // --top 1: the largest child is shown, the other summed in `rest`.
    let kids = doc["tree"]["children"].as_array().unwrap();
    assert_eq!(kids.len(), 1);
    assert_eq!(kids[0]["name"], "web");
    let rest = &doc["tree"]["rest"];
    assert_eq!(rest["count"], 1);
    // The rest is the root's own size: none on APFS, a block or so elsewhere.
    let own = total - kids[0]["disk"].as_u64().unwrap() - rest["disk"].as_u64().unwrap();
    assert!(own <= 16_384, "{own}");
    // The rules' match is on the entry.
    let nm = &kids[0]["children"][0];
    assert_eq!(nm["name"], "node_modules");
    assert_eq!(nm["class"]["rule"], "node.node_modules");
    assert_eq!(nm["class"]["category"], "dev.deps");

    let o = dfdu(&[
        "scan".as_ref(),
        root.as_os_str(),
        "--format".as_ref(),
        "ndjson".as_ref(),
    ]);
    assert!(o.status.success());
    let lines: Vec<Value> = o
        .stdout
        .split(|&b| b == b'\n')
        .filter(|l| !l.is_empty())
        .map(|l| serde_json::from_slice(l).unwrap())
        .collect();
    for l in &lines {
        valid(&schemas, ix[1], l);
    }
    assert_eq!(lines[0]["record"], "header");
    let last = lines.last().unwrap();
    assert_eq!(last["record"], "summary");
    assert_eq!(last["totals"]["disk"], total);
    let entries: Vec<&Value> = lines.iter().filter(|l| l["record"] == "entry").collect();
    assert_eq!(
        entries.len() as u64,
        last["totals"]["entries"].as_u64().unwrap()
    );
    // A name that is not UTF-8 round-trips through its base64.
    if let Some(e) = entries.iter().find(|e| e.get("path_bytes_b64").is_some()) {
        let raw = decode_b64(e["path_bytes_b64"].as_str().unwrap());
        assert!(raw.ends_with(b"/docs/caf\xe9.txt"), "{raw:?}");
        assert!(e["path"].as_str().unwrap().ends_with("caf\u{fffd}.txt"));
    } else if !cfg!(any(target_os = "macos", windows)) {
        panic!("no non-UTF-8 name");
    }
}

#[test]
fn report_json_validates_and_fail_above_sets_the_exit_code() {
    let tmp = tempfile::tempdir().unwrap();
    let root = dunce::canonicalize(tmp.path()).unwrap();
    fixture(&root);
    let (schemas, ix) = schemas();
    let report = |extra: &[&str]| {
        let mut args: Vec<&std::ffi::OsStr> = vec![
            "report".as_ref(),
            root.as_os_str(),
            "--format".as_ref(),
            "json".as_ref(),
        ];
        args.extend(extra.iter().map(std::ffi::OsStr::new));
        dfdu(&args)
    };
    let o = report(&[]);
    assert_eq!(
        o.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&o.stderr)
    );
    let doc = json(&o);
    valid(&schemas, ix[2], &doc);
    let items = doc["items"].as_array().unwrap();
    assert_eq!(items.len(), 1, "{items:?}");
    let it = &items[0];
    assert_eq!(it["rule"], "node.node_modules");
    assert_eq!(it["safety"], "regenerable");
    assert_eq!(it["path"], root.join("web/node_modules").to_str().unwrap());
    // Just written: not recommended (nothing changed in the last 7 days is).
    assert_eq!(it["recommended"], false);
    let size = it["disk"].as_u64().unwrap();
    assert!(size >= 40_000);
    assert_eq!(doc["summary"]["disk"], size);
    assert_eq!(doc["summary"]["by_safety"]["regenerable"]["items"], 1);

    let over = report(&["--fail-above", &(size - 1).to_string()]);
    assert_eq!(over.status.code(), Some(4));
    let doc = json(&over);
    valid(&schemas, ix[2], &doc);
    assert_eq!(doc["threshold"]["exceeded"], true);
    let within = report(&["--fail-above", &size.to_string()]);
    assert_eq!(within.status.code(), Some(0));
    assert_eq!(json(&within)["threshold"]["exceeded"], false);

    // Filters: a category not matched, a size above it, safety stricter than it.
    for f in [
        &["--category", "sys"][..],
        &["--min-size", "1G"],
        &["--safety", "safe"],
        &["--older-than", "30d"],
    ] {
        let doc = json(&report(f));
        valid(&schemas, ix[2], &doc);
        assert_eq!(doc["items"].as_array().unwrap().len(), 0, "{f:?}");
    }
    assert_eq!(
        json(&report(&["--category", "dev"]))["items"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert_eq!(report(&["--category", "nope"]).status.code(), Some(2));

    let table = dfdu(&["report".as_ref(), root.as_os_str()]);
    let text = String::from_utf8_lossy(&table.stdout);
    assert!(
        text.contains("node_modules") && text.contains("regenerable"),
        "{text}"
    );
}
