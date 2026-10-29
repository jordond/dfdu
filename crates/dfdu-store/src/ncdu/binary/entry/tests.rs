use super::super::tests::{V, entry, head};
use super::*;

fn parse(b: &[u8]) -> Result<RawItem, ImportError> {
    parse_with(b, &Limits::default())
}

fn parse_with(b: &[u8], limits: &Limits) -> Result<RawItem, ImportError> {
    Cbor {
        b,
        pos: 0,
        block: 3,
    }
    .item(limits)
}

fn msg(r: Result<RawItem, ImportError>) -> String {
    match r {
        Ok(_) => "parsed".into(),
        Err(e) => e.to_string(),
    }
}

fn item(typ: i64, rderr: bool, mode: Option<u64>) -> Item {
    Item {
        typ,
        info: Info {
            mode,
            ..Info::default()
        },
        rderr,
        prev: None,
        sub: None,
    }
}

#[test]
fn reads_the_fields_it_knows() {
    let b = entry(
        &[
            (0, V::I(3)),
            (1, V::S("name")),
            (2, V::I(-5)),
            (3, V::I(10)),
            (4, V::I(4096)),
            (5, V::I(77)),
            (12, V::I(7)),
            (13, V::I(42)),
            (14, V::I(2)),
            (17, V::I(0o100_644)),
            (18, V::I(1_700_000_000)),
        ],
        false,
    );
    let it = parse(&b).unwrap();
    assert_eq!(it.typ, 3);
    let i = &it.info;
    assert_eq!(i.name, b"name");
    assert_eq!(
        (i.asize, i.dsize, i.dev, i.ino, i.nlink),
        (10, 4096, Some(77), 42, 2)
    );
    assert_eq!((i.mode, i.mtime), (Some(0o100_644), 1_700_000_000));
    // A negative reference counts back from the entry: -1 is one byte back.
    assert!(matches!(it.prev, Some(Ref::Rel(5))));
    assert!(matches!(it.sub, Some(Ref::Abs(7))));
}

/// Keys it does not know are skipped whatever their value: strings, arrays and maps of
/// either length encoding, tags and simple values.
#[test]
fn unknown_keys_are_skipped() {
    let mut b = vec![0xbf];
    head(&mut b, 0, 1);
    head(&mut b, 3, 1);
    b.push(b'n');
    head(&mut b, 0, 100);
    head(&mut b, 2, 3);
    b.extend(b"xyz");
    head(&mut b, 0, 101);
    head(&mut b, 4, 2);
    head(&mut b, 1, 9);
    b.extend([0x9f, 0x01, 0xff]);
    head(&mut b, 0, 102);
    head(&mut b, 5, 1);
    head(&mut b, 0, 1);
    b.extend([0xa0]);
    head(&mut b, 0, 103);
    head(&mut b, 6, 1);
    head(&mut b, 0, 1_000_000);
    head(&mut b, 0, 104);
    b.push(0xf6);
    head(&mut b, 0, 0);
    head(&mut b, 1, 0);
    b.push(0xff);
    let it = parse(&b).unwrap();
    assert_eq!((it.typ, &it.info.name[..]), (-1, &b"n"[..]));
}

#[test]
fn malformed_entries_are_refused() {
    let named = |typ| entry(&[(0, V::I(typ)), (1, V::S("a"))], false);
    assert_eq!(msg(parse(&named(1))), "parsed");
    let cases: Vec<(Vec<u8>, &str)> = vec![
        (entry(&[(1, V::S("a"))], false), "without a type"),
        (entry(&[(0, V::I(1))], false), "without a name"),
        (vec![0x80], "not a map"),
        (entry(&[(0, V::S("x"))], false), "expected an integer"),
        (entry(&[(3, V::I(-1))], false), "expected an unsigned"),
        (entry(&[(6, V::I(1))], false), "expected true or false"),
        (entry(&[(2, V::S("x"))], false), "expected a reference"),
        (named(1)[..4].to_vec(), "cut short"),
        (vec![0xa1, 0x01, 0x7f, 0xff], "indefinite-length strings"),
        (vec![0xa1, 0x1c], "bad CBOR head"),
    ];
    for (b, want) in cases {
        let got = msg(parse(&b));
        assert!(got.contains(want), "{want}: {got}");
        assert!(got.contains("block 3"), "{got}");
    }
}

#[test]
fn names_and_nesting_have_limits() {
    let b = entry(&[(0, V::I(1)), (1, V::S("123456789"))], false);
    let short = Limits {
        max_name: 8,
        ..Limits::default()
    };
    assert!(msg(parse_with(&b, &short)).contains("a name of 9 bytes"));
    let nested = |n: usize| {
        let mut b = vec![0xa1, 0x18, 100];
        b.extend(std::iter::repeat_n(0x81, n));
        b.push(0x00);
        b
    };
    assert!(msg(parse(&nested(16))).contains("without a type"));
    assert!(msg(parse(&nested(17))).contains("nested too deep"));
}

/// Each entry type becomes the info block ncdu's JSON would have for it.
#[test]
fn types_become_the_json_info_block() {
    let (info, listed) = item(0, true, Some(0o100_644)).into_info();
    assert!(listed && info.read_error);
    assert_eq!(info.mode, Some(0o040_000), "a listed entry is a directory");
    let (info, listed) = item(1, true, Some(0o040_755)).into_info();
    assert!(!listed && !info.read_error);
    assert_eq!(
        info.mode, None,
        "an entry not listed is not a directory by its mode"
    );
    assert!(item(3, false, None).into_info().0.hlnkc);
    assert!(item(-1, false, None).into_info().0.read_error);
    for t in [2, 9] {
        assert!(item(t, false, None).into_info().0.notreg, "{t}");
    }
    for (t, why) in [
        (-2, "pattern"),
        (-3, "otherfs"),
        (-4, "kernfs"),
        (-9, "pattern"),
    ] {
        let (info, listed) = item(t, false, Some(0o100_644)).into_info();
        assert!(!listed);
        assert_eq!(info.excluded.as_deref(), Some(why.as_bytes()), "{t}");
        assert_eq!(info.mode, Some(0o100_644));
    }
}
