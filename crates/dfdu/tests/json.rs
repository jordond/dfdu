//! `dfdu scan --format json|ndjson` and `dfdu report --format json`: the output validates
//! against the schemas in `schema/`, its totals are the scan's, names that are not UTF-8 come
//! back from their base64, and `--fail-above` sets the exit code.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use std::fs;
use std::path::Path;
use std::process::Output;

use common::dfdu;
use serde_json::Value;

fn json(o: &Output) -> Value {
    serde_json::from_slice(&o.stdout).unwrap_or_else(|e| {
        panic!(
            "{e}: {}\n{}",
            String::from_utf8_lossy(&o.stdout),
            String::from_utf8_lossy(&o.stderr)
        )
    })
}

const SCHEMAS: [&str; 4] = [
    "scan.v1.json",
    "scan-ndjson.v1.json",
    "report.v1.json",
    "dupes.v1.json",
];

/// The published schemas, compiled.
fn schemas() -> (boon::Schemas, Vec<boon::SchemaIndex>) {
    common::schemas(&SCHEMAS, &SCHEMAS)
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
            .fold(0u32, |n, (i, &c)| n | u32::from(val(c)) << (18 - 6 * i));
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
    assert_eq!(
        it["path"],
        root.join("web").join("node_modules").to_str().unwrap()
    );
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

/// A native command is shown, in the table and in JSON, with the rule's variables filled in for
/// the item: the AVD rule deletes `avdmanager delete avd -n pixel`. The rule is verified on
/// macOS only, so it is off elsewhere.
#[test]
#[cfg(target_os = "macos")]
fn native_commands_are_shown_for_their_item() {
    let tmp = tempfile::tempdir().unwrap();
    let root = dunce::canonicalize(tmp.path()).unwrap();
    let avd = root.join("avd");
    fs::create_dir_all(avd.join("pixel.avd")).unwrap();
    fs::write(avd.join("pixel.avd/config.ini"), "x").unwrap();
    fs::write(avd.join("pixel.ini"), "x").unwrap();
    let report = |format: &str| {
        let o = common::bin_in(&root.join("state"))
            .args(["report".as_ref(), root.as_os_str()])
            .args(["--format", format])
            .env("ANDROID_AVD_HOME", &avd)
            .output()
            .unwrap();
        assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
        o
    };
    let table = String::from_utf8(report("table").stdout).unwrap();
    assert!(
        table.contains("`avdmanager delete avd -n pixel`"),
        "{table}"
    );
    let doc = json(&report("json"));
    let it = &doc["items"][0];
    assert_eq!(it["rule"], "android.avd", "{doc}");
    assert_eq!(
        it["native_command"],
        serde_json::json!(["avdmanager", "delete", "avd", "-n", "pixel"])
    );
}

/// A rule location behind a link that leads out of the scan is named in the report's table
/// footer and JSON, with where to run the report instead, and in the scan's JSON and NDJSON
/// summary; the scan does not follow the link.
#[test]
#[cfg(unix)]
fn rule_locations_behind_links_out_of_the_scan_are_named() {
    let tmp = tempfile::tempdir().unwrap();
    let top = dunce::canonicalize(tmp.path()).unwrap();
    let (home, data) = (top.join("home"), top.join("data/tool"));
    fs::create_dir_all(&home).unwrap();
    fs::create_dir_all(data.join("cache/v1")).unwrap();
    fs::write(data.join("cache/v1/blob"), vec![3u8; 30_000]).unwrap();
    std::os::unix::fs::symlink(&data, home.join(".tool")).unwrap();
    let rules = top.join("config/dfdu/rules");
    fs::create_dir_all(&rules).unwrap();
    fs::write(
        rules.join("tool.toml"),
        "schema_version = 1\n[[rule]]\nid = \"test.tool-cache\"\ncategory = \"dev.pkgcache\"\n\
         name = \"Tool caches\"\nsafety = \"safe\"\nexplanation = \"x\"\nlost = \"y\"\n\
         [rule.match]\nkind = \"dir\"\npaths = [\"{home}/.tool/cache\"]\n",
    )
    .unwrap();
    let run = |cmd: &str, root: &Path, format: &str| {
        let o = common::bin_in(&top.join("state"))
            .args([cmd.as_ref(), root.as_os_str()])
            .args(["--format", format])
            .env("HOME", &home)
            .env("XDG_CONFIG_HOME", top.join("config"))
            .output()
            .unwrap();
        assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
        o
    };
    let report = |root: &Path, format: &str| run("report", root, format);
    let table = String::from_utf8(report(&home, "table").stdout).unwrap();
    let d = data.display();
    assert!(
        table.contains(&format!(
            "~/.tool is a link to {d} (Tool caches), not scanned: dfdu report {d}"
        )),
        "{table}"
    );
    let doc = json(&report(&home, "json"));
    let (schemas, ix) = schemas();
    valid(&schemas, ix[2], &doc);
    let l = &doc["links_outside"][0];
    assert_eq!(l["link"], home.join(".tool").to_str().unwrap(), "{doc}");
    assert_eq!(l["target"], data.to_str().unwrap());
    assert_eq!(l["rules"], serde_json::json!(["test.tool-cache"]));
    // Scanned where it leads, the location is found, and there is no link to name.
    let doc = json(&report(&data, "json"));
    let found = doc["items"].as_array().unwrap();
    assert!(
        found.iter().any(|i| i["rule"] == "test.tool-cache"),
        "{doc}"
    );
    assert!(doc.get("links_outside").is_none(), "{doc}");
    // The scan names the same link: in its JSON document, and in NDJSON's summary record.
    let doc = json(&run("scan", &home, "json"));
    valid(&schemas, ix[0], &doc);
    assert_eq!(doc["links_outside"][0], *l, "{doc}");
    let o = run("scan", &home, "ndjson");
    let lines: Vec<Value> = (o.stdout.split(|&b| b == b'\n'))
        .filter(|l| !l.is_empty())
        .map(|l| serde_json::from_slice(l).unwrap())
        .collect();
    for line in &lines {
        valid(&schemas, ix[1], line);
    }
    let last = lines.last().unwrap();
    assert_eq!(last["record"], "summary");
    assert_eq!(last["links_outside"][0], *l, "{last}");
    assert!(last.get("not_entered").is_none(), "{last}");
}

#[test]
fn dupes_json_validates() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    let data: Vec<u8> = (0..200_000u32).map(|i| (i % 253) as u8).collect();
    fs::create_dir_all(root.join("a/b")).unwrap();
    fs::write(root.join("a/one.bin"), &data).unwrap();
    fs::write(root.join("a/b/two.bin"), &data).unwrap();
    fs::write(root.join("three.bin"), &data).unwrap();
    fs::write(root.join("other.bin"), vec![9u8; 200_000]).unwrap();
    let o = dfdu(&[
        "dupes".as_ref(),
        root.as_os_str(),
        "--min-size".as_ref(),
        "1KiB".as_ref(),
        "--format".as_ref(),
        "json".as_ref(),
    ]);
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let v = json(&o);
    let (schemas, ix) = schemas();
    valid(&schemas, ix[3], &v);
    assert_eq!(v["compared"], 4);
    let groups = v["groups"].as_array().unwrap();
    assert_eq!(groups.len(), 1);
    assert_eq!(groups[0]["files"].as_array().unwrap().len(), 3);
    assert_eq!(groups[0]["size"], 200_000);
}
