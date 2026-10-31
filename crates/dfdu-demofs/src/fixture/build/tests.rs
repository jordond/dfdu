use super::*;
use crate::FsError;
use crate::test_util::{SMALL, names, read_all, small};

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
    assert_eq!(t.nodes[locked].fault, Some(Fault::Denied));
    assert_eq!(t.nodes[locked].delay.as_millis(), 250);
    let pkg = t.resolve("dev/shop/package.json").unwrap();
    assert_eq!(t.nodes[pkg].nlink, 2);
    assert_eq!(t.resolve("dev/shop/hard").unwrap(), pkg);
    let link = t.resolve("dev/shop/link").unwrap();
    assert!(matches!(&t.nodes[link].kind, Kind::Symlink(b) if b == b"package.json"));
    assert_eq!(t.nodes[link].mode, 0o777);
    assert_eq!(t.nodes[pkg].mode, 0o644);
}

#[test]
fn same_content_reads_the_same_bytes() {
    let t = small();
    let a = t.resolve("Downloads/a.iso").unwrap();
    let a1 = t.resolve("Downloads/old/a (1).iso").unwrap();
    let b = t.resolve("Downloads/b.iso").unwrap();
    assert_eq!(read_all(&t, a), read_all(&t, a1));
    assert_ne!(read_all(&t, a), read_all(&t, b));
    let pkg = t.resolve("dev/shop/package.json").unwrap();
    assert_eq!(read_all(&t, pkg), b"{}");
}

#[test]
fn an_entry_sets_the_attributes_of_a_directory_made_for_an_earlier_path() {
    let f = Fixture::parse(
        r#"
now = "2026-10-01"

[[entry]]
path = "a/b/file"

[[entry]]
path = "a"
kind = "dir"
mode = 0o700
age = "1d"
error = "io"
"#,
    )
    .unwrap();
    let t = f.build().unwrap();
    let a = t.resolve("a").unwrap();
    assert_eq!(t.nodes[a].mode, 0o700);
    assert_eq!(t.nodes[a].mtime, f.now().unwrap() - 86_400);
    assert_eq!(t.nodes[a].fault, Some(Fault::Io));
    assert!(t.resolve("a/b/file").is_ok());
    assert_eq!(t.size(t.resolve("a/b/file").unwrap()), 0);
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
    assert!(bad(
        "[[entry]]\npath = \"d\"\nkind = \"dir\"\n[[entry]]\npath = \"l\"\nlink = \"d\""
    ));
    assert!(bad("[[entry]]\npath = \"a\"\nkind = \"symlink\""));
    assert!(bad("[[entry]]\npath = \"a\"\nage = \"old\""));
    assert!(bad(
        "[[entry]]\npath = \"a\"\nkind = \"dir\"\ndelay = \"soon\""
    ));
    assert!(bad("[[entry]]\npath = \"a\"\ncolour = \"red\""));
    assert!(!bad("[[entry]]\npath = \"a\""));
}

#[test]
fn a_failing_entry_is_named_in_the_error() {
    let f =
        Fixture::parse("now = \"2026-10-01\"\n[[entry]]\npath = \"x/y\"\nage = \"old\"").unwrap();
    let e = f.build().unwrap_err();
    assert!(format!("{e:#}").contains("entry \"x/y\""), "{e:#}");
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
    let nm = |t: &Tree, p: &str| names(t, t.resolve(p).unwrap());
    assert_eq!(
        nm(&alone, "dev/shop/node_modules"),
        nm(&t, "home/demo/dev/shop/node_modules")
    );
    assert_eq!(
        t.resolve("home/demo/dev/shop/hard").unwrap(),
        t.resolve("home/demo/dev/shop/package.json").unwrap()
    );
    assert_eq!(t.nodes[t.resolve("tmp").unwrap()].mode, 0o1777);
    // Not through a file.
    assert!(f.build_at(&mut t, "home/demo/sparse.img/x").is_err());
}

#[test]
fn mkdir_p_makes_what_is_missing_and_keeps_what_is_there() {
    let mut t = Tree::new(0, (1, 1));
    let b = mkdir_p(&mut t, 0, "a/b", 7).unwrap();
    let a = t.resolve("a").unwrap();
    assert_eq!(t.parent(b), a);
    assert_eq!(t.nodes[b].mtime, 7);
    assert_eq!(mkdir_p(&mut t, 0, "/a//b/", 9).unwrap(), b);
    assert_eq!(mkdir_p(&mut t, a, "b", 9).unwrap(), b);
    assert_eq!(mkdir_p(&mut t, 0, "", 9).unwrap(), 0);
    insert(&mut t, b, "f", Kind::seeded(1, 1), 0, 0o644).unwrap();
    assert!(mkdir_p(&mut t, 0, "a/b/f/g", 0).is_err());
    assert_eq!(t.resolve("a/b/f/g"), Err(FsError::NotDir));
    assert!(insert(&mut t, b, "f", Kind::dir(), 0, 0o755).is_err());
}

#[test]
fn parent_of_drops_the_last_component() {
    assert_eq!(parent_of("a/b/c"), "a/b");
    assert_eq!(parent_of("/lib/x.so"), "/lib");
    assert_eq!(parent_of("top"), "");
}
