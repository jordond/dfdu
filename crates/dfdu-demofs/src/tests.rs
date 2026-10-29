use crate::fixture::parse_duration;
use crate::{Data, Fixture, FsError, Kind, Tree, format_time, parse_size, parse_time};

const SMALL: &str = r#"
now = "2026-10-01T09:00:00Z"
age = "10d"

[disk]
total = "1TB"
free = "200GB"

[[entry]]
path = "Downloads/a.iso"
size = "1MiB"
content = "iso"
age = "200d"

[[entry]]
path = "Downloads/old/a (1).iso"
size = "1MiB"
content = "iso"

[[entry]]
path = "Downloads/b.iso"
size = "1MiB"

[[entry]]
path = "dev/shop/package.json"
text = "{}"

[[entry]]
path = "dev/shop/node_modules"
fill = { files = 50, size = "10MiB", style = "npm", seed = 3 }

[[entry]]
path = "dev/shop/link"
target = "package.json"

[[entry]]
path = "dev/shop/hard"
link = "dev/shop/package.json"

[[entry]]
path = "Library/locked"
kind = "dir"
error = "denied"
delay = "250ms"

[[entry]]
path = "sparse.img"
size = "10GiB"
alloc = "4KiB"
"#;

fn small() -> Tree {
    Fixture::parse(SMALL).unwrap().build().unwrap()
}

fn read_all(t: &Tree, n: usize) -> Vec<u8> {
    t.read(n, 0, u32::try_from(t.size(n)).unwrap()).unwrap()
}

#[test]
fn parses_sizes_times_and_durations() {
    assert_eq!(parse_size("1.5KiB").unwrap(), 1536);
    assert_eq!(parse_size("2GB").unwrap(), 2_000_000_000);
    assert_eq!(parse_size("1_000").unwrap(), 1000);
    assert!(parse_size("3 parsecs").is_err());
    assert_eq!(parse_time("1970-01-02").unwrap(), 86_400);
    let t = parse_time("2026-10-01T09:00:00Z").unwrap();
    assert_eq!(format_time(t), "2026-10-01 09:00:00");
    assert_eq!(parse_time("2026-10-01 09:00:00").unwrap(), t);
    assert_eq!(parse_duration("300ms").unwrap().as_millis(), 300);
    assert_eq!(parse_duration("2w").unwrap().as_secs(), 14 * 86_400);
    // Overflow, and dates and times that do not exist, are errors rather than wrong values.
    assert!(parse_size("20000000000000000000TiB").is_err());
    assert!(parse_duration("99999999999999999y").is_err());
    assert!(parse_time("2026-02-31").is_err());
    assert!(parse_time("2026-02-28T25:00:00").is_err());
    assert!(parse_time("2026-13-01").is_err());
    assert!(parse_time("2024-02-29").is_ok());
}

#[test]
fn builds_entries_with_their_sizes_and_ages() {
    let f = Fixture::parse(SMALL).unwrap();
    let now = f.now().unwrap();
    let t = f.build().unwrap();
    let iso = t.resolve("Downloads/a.iso").unwrap();
    assert_eq!(t.size(iso), 1 << 20);
    assert_eq!(t.alloc(iso), 1 << 20);
    assert_eq!(t.nodes[iso].mtime, now - 200 * 86_400);
    // Made for the path: the default age.
    let old = t.resolve("Downloads/old").unwrap();
    assert_eq!(t.nodes[old].mtime, now - 10 * 86_400);
    let sparse = t.resolve("sparse.img").unwrap();
    assert_eq!((t.size(sparse), t.alloc(sparse)), (10 << 30, 4096));
    let locked = t.resolve("Library/locked").unwrap();
    assert!(t.nodes[locked].fault.is_some());
    assert_eq!(t.nodes[locked].delay.as_millis(), 250);
    let pkg = t.resolve("dev/shop/package.json").unwrap();
    assert_eq!(t.nodes[pkg].nlink, 2);
    assert_eq!(t.resolve("dev/shop/hard").unwrap(), pkg);
    let link = t.resolve("dev/shop/link").unwrap();
    assert!(matches!(&t.nodes[link].kind, Kind::Symlink(b) if b == b"package.json"));
}

#[test]
fn fill_makes_its_files_adding_up_to_its_size_the_same_every_time() {
    let t = small();
    let nm = t.resolve("dev/shop/node_modules").unwrap();
    let mut files = 0;
    let mut bytes = 0;
    let mut stack = vec![nm];
    while let Some(d) = stack.pop() {
        for e in t.list(d).unwrap() {
            if t.nodes[e.node].is_dir() {
                stack.push(e.node);
            } else {
                files += 1;
                bytes += t.size(e.node);
            }
        }
    }
    assert_eq!((files, bytes), (50, 10 << 20));
    assert!(
        t.resolve("dev/shop/node_modules/react/package.json")
            .is_ok()
    );
    let again = small();
    assert_eq!(t.nodes.len(), again.nodes.len());
    for n in 0..t.nodes.len() {
        assert_eq!(t.size(n), again.size(n));
        assert_eq!(t.nodes[n].mtime, again.nodes[n].mtime);
    }
}

#[test]
fn same_content_reads_the_same_bytes() {
    let t = small();
    let a = t.resolve("Downloads/a.iso").unwrap();
    let a1 = t.resolve("Downloads/old/a (1).iso").unwrap();
    let b = t.resolve("Downloads/b.iso").unwrap();
    assert_eq!(read_all(&t, a), read_all(&t, a1));
    assert_ne!(read_all(&t, a), read_all(&t, b));
    // Reads at any offset agree with the whole.
    let whole = read_all(&t, a);
    assert_eq!(t.read(a, 13, 100).unwrap(), whole[13..113]);
    assert_eq!(t.read(a, (1 << 20) - 3, 100).unwrap().len(), 3);
    let pkg = t.resolve("dev/shop/package.json").unwrap();
    assert_eq!(read_all(&t, pkg), b"{}");
}

#[test]
fn bad_fixtures_are_refused() {
    let bad = |e: &str| {
        let text = format!("now = \"2026-10-01\"\n{e}");
        Fixture::parse(&text).and_then(|f| f.build()).is_err()
    };
    assert!(bad("[[entry]]\npath = \"a\"\nsize = \"1 lightyear\""));
    assert!(bad("[[entry]]\npath = \"a\"\n[[entry]]\npath = \"a/b\""));
    assert!(bad("[[entry]]\npath = \"a\"\n[[entry]]\npath = \"a\""));
    assert!(bad("[[entry]]\npath = \"a\"\nlink = \"missing\""));
    assert!(bad("[[entry]]\npath = \"a\"\ncolour = \"red\""));
    assert!(Fixture::parse("now = \"yesterday\"").is_err());
}

#[test]
fn unlink_rmdir_and_used_space() {
    let mut t = small();
    let used = t.used();
    let dl = t.resolve("Downloads").unwrap();
    assert_eq!(t.rmdir(dl, b"old", 0), Err(FsError::NotEmpty));
    assert_eq!(t.unlink(dl, b"old", 0), Err(FsError::IsDir));
    let old = t.resolve("Downloads/old").unwrap();
    t.unlink(old, b"a (1).iso", 5).unwrap();
    t.rmdir(dl, b"old", 5).unwrap();
    assert_eq!(t.resolve("Downloads/old"), Err(FsError::NoEnt));
    assert_eq!(t.used(), used - (1 << 20) - 4096);
    assert_eq!(t.nodes[dl].mtime, 5);
    // A file with two links stays until the last goes.
    let shop = t.resolve("dev/shop").unwrap();
    let pkg = t.resolve("dev/shop/package.json").unwrap();
    t.unlink(shop, b"package.json", 0).unwrap();
    assert!(t.get(pkg).is_some());
    t.unlink(shop, b"hard", 0).unwrap();
    assert!(t.get(pkg).is_none());
}

#[test]
fn rename_moves_replaces_and_refuses_as_rename2_does() {
    let mut t = small();
    let dl = t.resolve("Downloads").unwrap();
    let root = 0;
    // NOREPLACE onto an existing name.
    assert_eq!(
        t.rename(dl, b"a.iso", dl, b"b.iso", true, false, 0),
        Err(FsError::Exist)
    );
    // A directory into itself.
    let old = t.resolve("Downloads/old").unwrap();
    assert_eq!(
        t.rename(root, b"Downloads", old, b"x", false, false, 0),
        Err(FsError::Inval)
    );
    // Replacing a file.
    let a = t.resolve("Downloads/a.iso").unwrap();
    t.rename(dl, b"a.iso", dl, b"b.iso", false, false, 0)
        .unwrap();
    assert_eq!(t.resolve("Downloads/b.iso").unwrap(), a);
    assert_eq!(t.resolve("Downloads/a.iso"), Err(FsError::NoEnt));
    // Moving a directory updates its parent.
    let trash = t.insert(root, b"Trash", Kind::dir(), 0, 0o755).unwrap();
    t.rename(dl, b"old", trash, b"old", true, false, 0).unwrap();
    assert_eq!(t.parent(old), trash);
    // Exchange.
    let shop = t.resolve("dev/shop").unwrap();
    let pkg = t.resolve("dev/shop/package.json").unwrap();
    let link = t.resolve("dev/shop/link").unwrap();
    t.rename(shop, b"package.json", shop, b"link", false, true, 0)
        .unwrap();
    assert_eq!(t.resolve("dev/shop/link").unwrap(), pkg);
    assert_eq!(t.resolve("dev/shop/package.json").unwrap(), link);
}

#[test]
fn writes_and_truncates() {
    let mut t = small();
    let root = 0;
    let n = t
        .insert(
            root,
            b"note.trashinfo",
            Kind::File {
                size: 0,
                alloc: None,
                data: Data::Bytes(vec![]),
            },
            0,
            0o644,
        )
        .unwrap();
    t.write(n, 0, b"[Trash Info]\n", 7).unwrap();
    t.write(n, 13, b"Path=/x\n", 7).unwrap();
    assert_eq!(read_all(&t, n), b"[Trash Info]\nPath=/x\n");
    assert_eq!(t.nodes[n].mtime, 7);
    t.truncate(n, 5, 8).unwrap();
    assert_eq!(read_all(&t, n), b"[Tras");
    // A small generated file becomes stored bytes when written, keeping the rest.
    let a = t.resolve("Downloads/a.iso").unwrap();
    let before = read_all(&t, a);
    t.write(a, 0, b"XY", 9).unwrap();
    let after = read_all(&t, a);
    assert_eq!(&after[..2], b"XY");
    assert_eq!(after[2..], before[2..]);
    // A large one is refused.
    let sparse = t.resolve("sparse.img").unwrap();
    assert_eq!(t.write(sparse, 0, b"x", 0), Err(FsError::TooBig));
}

#[test]
fn a_grown_file_reads_zeros_after_its_old_end() {
    let mut t = small();
    let a = t.resolve("Downloads/a.iso").unwrap();
    let head = t.read(a, 0, 64).unwrap();
    t.truncate(a, 32, 0).unwrap();
    t.truncate(a, 96, 0).unwrap();
    let b = read_all(&t, a);
    assert_eq!(b[..32], head[..32]);
    assert!(b[32..].iter().all(|&x| x == 0));
}

#[test]
fn cookies_grow_and_stay_with_their_entries() {
    let mut t = small();
    let dl = t.resolve("Downloads").unwrap();
    let cookies = |t: &Tree| {
        t.list(dl)
            .unwrap()
            .iter()
            .map(|e| e.cookie)
            .collect::<Vec<_>>()
    };
    let before = cookies(&t);
    assert!(before.windows(2).all(|w| w[0] < w[1]));
    // Removing one keeps the others' cookies; a hard link in the same directory gets its own.
    t.unlink(dl, b"b.iso", 0).unwrap();
    let a = t.resolve("Downloads/a.iso").unwrap();
    t.link(a, dl, b"a-again.iso").unwrap();
    let after = cookies(&t);
    assert!(after.windows(2).all(|w| w[0] < w[1]));
    assert!(before.iter().filter(|c| after.contains(c)).count() == before.len() - 1);
}

#[test]
fn a_failed_insert_leaves_nothing_behind() {
    let mut t = small();
    let nodes = t.nodes.len();
    let file = t.resolve("Downloads/a.iso").unwrap();
    assert_eq!(
        t.insert(file, b"x", Kind::dir(), 0, 0o755),
        Err(FsError::NotDir)
    );
    assert_eq!(
        t.insert(0, b"Downloads", Kind::dir(), 0, 0o755),
        Err(FsError::Exist)
    );
    assert_eq!(t.nodes.len(), nodes);
}

#[test]
fn a_fixture_built_into_another_keeps_its_bytes_and_owner() {
    let base = Fixture::parse(
        r#"
now = "2026-10-01"
uid = 0
gid = 0

[[entry]]
path = "home"
kind = "dir"

[[entry]]
path = "tmp"
kind = "dir"
mode = 0o1777
"#,
    )
    .unwrap();
    let mut t = base.build().unwrap();
    let f = Fixture::parse(SMALL).unwrap();
    f.build_at(&mut t, "home/demo").unwrap();
    let alone = small();
    let owner = |n: usize| (t.nodes[n].uid, t.nodes[n].gid);
    assert_eq!(owner(0), (0, 0));
    assert_eq!(owner(t.resolve("home").unwrap()), (0, 0));
    let home = t.resolve("home/demo").unwrap();
    assert_eq!(owner(home), (1000, 1000));
    assert_eq!(t.nodes[home].mtime, alone.nodes[0].mtime);
    // The same bytes, fills and links as the fixture on its own.
    for p in ["Downloads/a.iso", "dev/shop/package.json"] {
        let (a, b) = (
            alone.resolve(p).unwrap(),
            t.resolve(&format!("home/demo/{p}")).unwrap(),
        );
        assert_eq!(read_all(&alone, a), read_all(&t, b));
        assert_eq!(owner(b), (1000, 1000));
    }
    let nm = |t: &Tree, p: &str| {
        let d = t.resolve(p).unwrap();
        t.list(d)
            .unwrap()
            .iter()
            .map(|e| e.name.clone())
            .collect::<Vec<_>>()
    };
    assert_eq!(
        nm(&alone, "dev/shop/node_modules"),
        nm(&t, "home/demo/dev/shop/node_modules")
    );
    assert_eq!(
        t.resolve("home/demo/dev/shop/hard").unwrap(),
        t.resolve("home/demo/dev/shop/package.json").unwrap()
    );
    assert_eq!(t.nodes[t.resolve("tmp").unwrap()].mode, 0o1777);
}

#[cfg(unix)]
#[test]
fn imports_copy_files_links_and_directories() {
    use std::os::unix::fs::PermissionsExt as _;
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("sys");
    std::fs::create_dir_all(src.join("lib")).unwrap();
    std::fs::write(src.join("tool"), b"#!/bin/sh\n").unwrap();
    std::fs::set_permissions(src.join("tool"), std::fs::Permissions::from_mode(0o755)).unwrap();
    std::fs::write(src.join("lib/b.so"), b"b").unwrap();
    std::fs::write(src.join("lib/a.so"), b"a").unwrap();
    std::os::unix::fs::symlink("a.so", src.join("lib/a.so.1")).unwrap();
    let text = format!(
        "now = \"2026-10-01\"\nage = \"5d\"\nuid = 0\nimport = [{:?}, {:?}]\n",
        src.join("tool").display(),
        src.join("lib").display()
    );
    let f = Fixture::parse(&text).unwrap();
    let t = f.build().unwrap();
    let at = |p: &str| {
        let full = src.join(p);
        t.resolve(full.to_str().unwrap()).unwrap()
    };
    let tool = at("tool");
    assert_eq!(read_all(&t, tool), b"#!/bin/sh\n");
    assert_eq!(t.nodes[tool].mode, 0o755);
    assert_eq!(t.nodes[tool].uid, 0);
    assert_eq!(t.nodes[tool].mtime, f.now().unwrap() - 5 * 86_400);
    assert!(matches!(&t.nodes[at("lib/a.so.1")].kind, Kind::Symlink(b) if b == b"a.so"));
    let names: Vec<_> = t
        .list(at("lib"))
        .unwrap()
        .iter()
        .map(|e| e.name.clone())
        .collect();
    assert_eq!(names, [&b"a.so"[..], b"a.so.1", b"b.so"]);
    assert_eq!(read_all(&t, at("lib/b.so")), b"b");
    // A path that is not there is an error, not an empty import.
    let missing = format!(
        "now = \"2026-10-01\"\nimport = [{:?}]\n",
        src.join("gone").display()
    );
    assert!(Fixture::parse(&missing).unwrap().build().is_err());
}

#[cfg(unix)]
#[test]
fn import_patterns_take_every_match_and_fail_on_none() {
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("sys");
    for arch in ["aarch64-linux-gnu", "x86_64-linux-gnu", "other"] {
        std::fs::create_dir_all(src.join(arch)).unwrap();
        std::fs::write(src.join(arch).join("libc.so.6"), arch).unwrap();
    }
    let build = |pattern: &str| {
        let text = format!(
            "now = \"2026-10-01\"\nimport = [{:?}]\n",
            format!("{}/{pattern}", src.display())
        );
        Fixture::parse(&text).unwrap().build()
    };
    let t = build("*-linux-*/lib*.so.6").unwrap();
    let at = |p: &str| t.resolve(src.join(p).to_str().unwrap());
    assert_eq!(read_all(&t, at("aarch64-linux-gnu/libc.so.6").unwrap()), b"aarch64-linux-gnu");
    assert_eq!(read_all(&t, at("x86_64-linux-gnu/libc.so.6").unwrap()), b"x86_64-linux-gnu");
    assert!(at("other/libc.so.6").is_err());
    assert!(build("*-linux-gnu/libm.so.6").is_err());
}

#[test]
fn a_fixture_extends_another_removing_and_replacing_entries() {
    let week_ago = r#"
extends = "small.toml"
age = "20d"
remove = ["Downloads/old", "sparse.img"]

[[entry]]
path = "Downloads/b.iso"
size = "2MiB"

[[entry]]
path = "notes.txt"
text = "new"
"#;
    let f = Fixture::parse_over(week_ago, |name| {
        assert_eq!(name, "small.toml");
        Fixture::parse(SMALL)
    })
    .unwrap();
    let base = Fixture::parse(SMALL).unwrap();
    assert_eq!(f.now, base.now);
    assert_eq!(f.age, "20d");
    assert_eq!(
        f.disk.total.bytes().unwrap(),
        base.disk.total.bytes().unwrap()
    );
    let t = f.build().unwrap();
    assert!(t.resolve("Downloads/old").is_err());
    assert!(t.resolve("sparse.img").is_err());
    // Replaced where it was, so the listing keeps its order.
    let dl = t.resolve("Downloads").unwrap();
    let names: Vec<_> = t.list(dl).unwrap().iter().map(|e| e.name.clone()).collect();
    assert_eq!(names, [&b"a.iso"[..], b"b.iso"]);
    assert_eq!(t.size(t.resolve("Downloads/b.iso").unwrap()), 2 << 20);
    // Inherited entries keep their bytes and fills.
    let alone = small();
    let a = |t: &Tree| read_all(t, t.resolve("Downloads/a.iso").unwrap());
    assert_eq!(a(&t), a(&alone));
    assert_eq!(read_all(&t, t.resolve("notes.txt").unwrap()), b"new");

    // Removing what is not there is a mistake, and so is extending in plain text.
    let typo = "extends = \"x\"\nremove = [\"Dowloads\"]\n";
    assert!(Fixture::parse_over(typo, |_| Fixture::parse(SMALL)).is_err());
    assert!(Fixture::parse(week_ago).is_err());
    assert!(Fixture::parse("age = \"1d\"\n").is_err());
}

#[test]
fn fixtures_that_extend_each_other_in_a_loop_are_refused() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("a.toml"), "extends = \"b.toml\"\n").unwrap();
    std::fs::write(dir.path().join("b.toml"), "extends = \"a.toml\"\n").unwrap();
    assert!(Fixture::load(&dir.path().join("a.toml")).is_err());
    std::fs::write(dir.path().join("c.toml"), "now = \"2026-10-01\"\n").unwrap();
    std::fs::write(dir.path().join("d.toml"), "extends = \"c.toml\"\nuid = 5\n").unwrap();
    let d = Fixture::load(&dir.path().join("d.toml")).unwrap();
    assert_eq!((d.now.as_str(), d.uid), ("2026-10-01", 5));
}
