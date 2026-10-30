use super::*;
use crate::Tree;
use crate::test_support::{SMALL, names, read_all, small};

#[test]
fn parses_the_keys_and_defaults_the_rest() {
    let f = Fixture::parse(SMALL).unwrap();
    assert_eq!(f.now, "2026-10-01T09:00:00Z");
    assert_eq!(f.age, "10d");
    assert_eq!((f.uid, f.gid), (1000, 1000));
    assert_eq!(f.disk.total.bytes().unwrap(), 1_000_000_000_000);
    assert_eq!(f.disk.free.bytes().unwrap(), 200_000_000_000);
    assert_eq!(f.disk.device, "dfdu-demofs");
    assert_eq!(f.entries.len(), 9);
    assert_eq!(
        f.now().unwrap(),
        parse_time("2026-10-01T09:00:00Z").unwrap()
    );
    let bare = Fixture::parse("now = \"2026-10-01\"\n[disk]\ntotal = 1000\n").unwrap();
    assert_eq!(bare.age, "30d");
    assert_eq!(bare.disk.total.bytes().unwrap(), 1000);
    assert_eq!(bare.disk.free.bytes().unwrap(), 200_000_000_000);
    assert!(bare.import.is_empty() && bare.entries.is_empty());
}

#[test]
fn a_fixture_needs_a_time_it_can_read() {
    assert!(Fixture::parse("age = \"1d\"\n").is_err());
    assert!(Fixture::parse("now = \"yesterday\"").is_err());
    assert!(Fixture::parse("now = \"2026-10-01\"\ncolour = \"red\"").is_err());
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
        Ok(Fixture::parse(SMALL)?)
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
    assert_eq!(names(&t, dl), [&b"a.iso"[..], b"b.iso"]);
    assert_eq!(t.size(t.resolve("Downloads/b.iso").unwrap()), 2 << 20);
    // Inherited entries keep their bytes and fills.
    let alone = small();
    let a = |t: &Tree| read_all(t, t.resolve("Downloads/a.iso").unwrap());
    assert_eq!(a(&t), a(&alone));
    assert_eq!(read_all(&t, t.resolve("notes.txt").unwrap()), b"new");

    // Removing what is not there is a mistake, and so is extending in plain text.
    let typo = "extends = \"x\"\nremove = [\"Dowloads\"]\n";
    assert!(Fixture::parse_over(typo, |_| Ok(Fixture::parse(SMALL)?)).is_err());
    assert!(Fixture::parse(week_ago).is_err());
}

#[test]
fn fixtures_that_extend_each_other_in_a_loop_are_refused() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("a.toml"), "extends = \"b.toml\"\n").unwrap();
    std::fs::write(dir.path().join("b.toml"), "extends = \"a.toml\"\n").unwrap();
    let e = Fixture::load(&dir.path().join("a.toml")).unwrap_err();
    assert!(format!("{e:#}").contains("extend one another"), "{e:#}");
    std::fs::write(dir.path().join("c.toml"), "now = \"2026-10-01\"\n").unwrap();
    std::fs::write(dir.path().join("d.toml"), "extends = \"c.toml\"\nuid = 5\n").unwrap();
    let d = Fixture::load(&dir.path().join("d.toml")).unwrap();
    assert_eq!((d.now.as_str(), d.uid), ("2026-10-01", 5));
}

#[test]
fn a_missing_file_names_its_path() {
    let dir = tempfile::tempdir().unwrap();
    let e = Fixture::load(&dir.path().join("gone.toml")).unwrap_err();
    assert!(e.to_string().contains("gone.toml"), "{e}");
}
