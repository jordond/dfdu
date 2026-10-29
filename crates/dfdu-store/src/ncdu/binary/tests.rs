use std::io;

use super::*;

/// `ncdu -x -e -O` of `s/{a, ln -> a, sub/{b, hl}}`, `hl` a hard link to `a`.
const SAMPLE: &[u8] = include_bytes!("sample.bin");

fn read(b: &[u8]) -> Result<Imported, ImportError> {
    import(io::Cursor::new(b), &Limits::default())
}

#[test]
fn reads_ncdus_sample() {
    let im = read(SAMPLE).unwrap();
    assert_eq!(im.entries, 6);
    let t = im.tree.total(im.root);
    assert_eq!(t.items, 6);
    assert!(im.tree.name(im.root).ends_with(b"/s"));
}

#[test]
fn every_cut_is_refused_and_no_byte_flip_panics() {
    for n in 0..SAMPLE.len() {
        assert!(read(&SAMPLE[..n]).is_err(), "cut at {n}");
    }
    for i in 0..SAMPLE.len() {
        for bit in 0..8 {
            let mut b = SAMPLE.to_vec();
            b[i] ^= 1 << bit;
            let _ = read(&b);
        }
    }
}

/// CBOR head.
#[expect(
    clippy::decimal_bitwise_operands,
    reason = "RFC 8949 gives the additional information values (24, 25, 27) in decimal"
)]
pub(super) fn head(out: &mut Vec<u8>, major: u8, n: u64) {
    let m = major << 5;
    match n {
        0..=23 => out.push(m | u8::try_from(n).unwrap()),
        24..=0xff => out.extend([m | 24, u8::try_from(n).unwrap()]),
        0x100..=0xffff => {
            out.push(m | 25);
            out.extend(u16::try_from(n).unwrap().to_be_bytes());
        }
        _ => {
            out.push(m | 27);
            out.extend(n.to_be_bytes());
        }
    }
}

pub(super) enum V<'a> {
    I(i64),
    S(&'a str),
}

/// An entry as a CBOR map, `indefinite` with a break at the end.
pub(super) fn entry(fields: &[(u64, V)], indefinite: bool) -> Vec<u8> {
    let mut out = vec![];
    if indefinite {
        out.push(0xbf);
    } else {
        head(&mut out, 5, fields.len() as u64);
    }
    for (k, v) in fields {
        head(&mut out, 0, *k);
        match v {
            V::I(i) if *i >= 0 => head(&mut out, 0, u64::try_from(*i).unwrap()),
            V::I(i) => head(&mut out, 1, u64::try_from(-1 - *i).unwrap()),
            V::S(s) => {
                head(&mut out, 3, s.len() as u64);
                out.extend(s.as_bytes());
            }
        }
    }
    if indefinite {
        out.push(0xff);
    }
    out
}

/// A file of the given decompressed data blocks and root reference.
fn file(blocks: &[Vec<u8>], root: u64) -> Vec<u8> {
    let mut out = SIGNATURE.to_vec();
    let mut ptrs = vec![];
    for (n, data) in blocks.iter().enumerate() {
        let z = ruzstd::encoding::compress_to_vec(
            &data[..],
            ruzstd::encoding::CompressionLevel::Fastest,
        );
        let len = u32::try_from(z.len() + 12).unwrap();
        ptrs.push(((out.len() as u64) << 24) | u64::from(len));
        out.extend(len.to_be_bytes());
        out.extend(u32::try_from(n).unwrap().to_be_bytes());
        out.extend(z);
        out.extend(len.to_be_bytes());
    }
    let len = u32::try_from(8 + ptrs.len() * 8 + 8).unwrap() | (1 << 28);
    out.extend(len.to_be_bytes());
    for p in ptrs {
        out.extend(p.to_be_bytes());
    }
    out.extend(root.to_be_bytes());
    out.extend(len.to_be_bytes());
    out
}

/// `r/{a, b, d/{c}}` over two blocks, with relative and absolute references and both map
/// encodings.
#[test]
fn reads_relative_and_absolute_references_across_blocks() {
    let mut b0 = entry(
        &[(0, V::I(1)), (1, V::S("a")), (3, V::I(10)), (4, V::I(4096))],
        false,
    );
    let b_at = b0.len();
    b0.extend(entry(
        &[
            (1, V::S("b")),
            (0, V::I(1)),
            (3, V::I(20)),
            (2, V::I(-i64::try_from(b_at).unwrap())),
        ],
        true,
    ));
    let c = entry(&[(0, V::I(1)), (1, V::S("c")), (3, V::I(30))], false);
    let mut b1 = c.clone();
    let d_at = b1.len();
    b1.extend(entry(
        &[
            (0, V::I(0)),
            (1, V::S("d")),
            (12, V::I(-i64::try_from(d_at).unwrap())),
            (2, V::I(i64::try_from(b_at).unwrap())),
        ],
        false,
    ));
    let r_at = b1.len();
    b1.extend(entry(
        &[
            (0, V::I(0)),
            (1, V::S("r")),
            (12, V::I(i64::try_from((1 << 24) | d_at).unwrap())),
        ],
        false,
    ));
    let im = read(&file(&[b0, b1], (1 << 24) | r_at as u64)).unwrap();
    let t = im.tree.total(im.root);
    assert_eq!((t.items, t.apparent), (5, 60));
}

#[test]
fn loops_and_bad_references_are_refused() {
    // `a`'s prev is itself.
    let mut b0 = entry(&[(0, V::I(1)), (1, V::S("a")), (2, V::I(0))], false);
    let r_at = b0.len();
    b0.extend(entry(&[(0, V::I(0)), (1, V::S("r")), (12, V::I(0))], false));
    let e = read(&file(&[b0], r_at as u64)).err().unwrap();
    assert!(e.to_string().contains("reached twice"), "{e}");

    // A directory that contains itself.
    let b0 = entry(&[(0, V::I(0)), (1, V::S("r")), (12, V::I(0))], false);
    assert!(read(&file(&[b0], 0)).is_err());

    // References past the block, before it, and to a missing block.
    for sub in [1000, -1000, 5 << 24] {
        let b0 = entry(&[(0, V::I(0)), (1, V::S("r")), (12, V::I(sub))], false);
        assert!(read(&file(&[b0], 0)).is_err(), "{sub}");
    }

    // A root that is not a directory, and names that cannot be.
    let b0 = entry(&[(0, V::I(1)), (1, V::S("r"))], false);
    assert!(read(&file(&[b0], 0)).is_err());
    for name in ["", ".", "..", "x/y"] {
        let mut b0 = entry(&[(0, V::I(1)), (1, V::S(name))], false);
        let r_at = b0.len();
        b0.extend(entry(&[(0, V::I(0)), (1, V::S("r")), (12, V::I(0))], false));
        assert!(read(&file(&[b0], r_at as u64)).is_err(), "{name:?}");
    }
}

#[test]
fn limits_hold() {
    // A chain of directories deeper than allowed.
    let mut b0 = entry(&[(0, V::I(0)), (1, V::S("d"))], false);
    let mut at = 0;
    for _ in 0..10 {
        let next = b0.len();
        b0.extend(entry(
            &[(0, V::I(0)), (1, V::S("d")), (12, V::I(at))],
            false,
        ));
        at = i64::try_from(next).unwrap();
    }
    let f = file(&[b0], u64::try_from(at).unwrap());
    let lim = Limits {
        max_depth: 4,
        ..Default::default()
    };
    assert!(matches!(
        import(io::Cursor::new(&f), &lim),
        Err(ImportError::Limit(_))
    ));
    assert!(read(&f).is_ok());
    let lim = Limits {
        max_entries: 5,
        ..Default::default()
    };
    assert!(matches!(
        import(io::Cursor::new(&f), &lim),
        Err(ImportError::Limit(_))
    ));
}
