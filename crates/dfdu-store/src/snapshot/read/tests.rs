use super::super::tests::{bytes, header, sample};
use super::super::write::{write, write_as};
use super::*;

fn rec(b: &[u8]) -> Rec<&[u8]> {
    Rec(b)
}

fn msg(e: ImportError) -> String {
    match e {
        ImportError::Format { msg, .. } | ImportError::Limit(msg) => msg,
        ImportError::Io(e) => e.to_string(),
    }
}

/// Every flipped byte and every cut is refused, never half-loaded.
#[test]
fn damage_is_refused() {
    let (t, r) = sample();
    let out = bytes(&t, r);
    for i in 0..out.len() {
        let mut bad = out.clone();
        bad[i] ^= 0x41;
        assert!(read(&bad[..], &Limits::default()).is_err(), "flip at {i}");
        assert!(read(&out[..i], &Limits::default()).is_err(), "cut at {i}");
    }
    let mut more = out.clone();
    more.push(0);
    assert!(read(&more[..], &Limits::default()).is_err());
}

/// Version 1 files (no journal position) and version 2 files (no time) still read.
#[test]
fn version_1_reads() {
    let (t, r) = sample();
    let mut out = vec![];
    let h = Header {
        journal: None,
        ..header()
    };
    write(&t, r, &mut out, &h).unwrap();
    out[8..12].copy_from_slice(&1u32.to_le_bytes());
    let l = read(&out[..], &Limits::default()).unwrap();
    assert_eq!(l.header, h);
    let l = read(&bytes(&t, r)[..], &Limits::default()).unwrap();
    assert_eq!(l.header, header());
    // Version 2: a journal position without its time.
    let mut v2 = header();
    v2.journal.as_mut().unwrap().time = 0;
    let mut out = vec![];
    write_as(&t, r, &mut out, &v2, VERSION_2).unwrap();
    let l = read(&out[..], &Limits::default()).unwrap();
    assert_eq!(l.header, v2);
}

/// A format version this build does not know is refused, and so is a file without the magic.
#[test]
fn unknown_versions_and_other_files_are_refused() {
    let (t, r) = sample();
    let mut out = bytes(&t, r);
    out[8..12].copy_from_slice(&(VERSION + 1).to_le_bytes());
    assert!(read(&out[..], &Limits::default()).is_err());
    assert!(!is_snapshot(b"[1,2,{}"));
    assert!(read(&b"[1,2,{},[{\"name\":\"/\"}]]"[..], &Limits::default()).is_err());
}

#[test]
fn varints_read_little_end_first_and_signed_ones_zigzagged() {
    assert_eq!(rec(&[0]).u().unwrap(), 0);
    assert_eq!(rec(&[0x7f]).u().unwrap(), 127);
    assert_eq!(rec(&[0xac, 0x02]).u().unwrap(), 300);
    let mut max = vec![0xff; 9];
    max.push(0x01);
    assert_eq!(rec(&max).u().unwrap(), u64::MAX);
    for (b, v) in [(0, 0), (1, -1), (2, 1), (3, -2)] {
        assert_eq!(rec(&[b]).i().unwrap(), v);
    }
    assert_eq!(rec(&max).i().unwrap(), i64::MIN);
}

#[test]
fn a_number_past_ten_bytes_or_cut_short_is_refused() {
    let long = [0x80; 10];
    assert_eq!(msg(rec(&long).u().unwrap_err()), "a number too long");
    assert_eq!(msg(rec(&[0x80]).u().unwrap_err()), "the records end early");
}

#[test]
fn byte_strings_hold_to_their_limit() {
    assert_eq!(rec(&[3, b'a', b'b', b'c']).bytes(3).unwrap(), b"abc");
    assert!(matches!(
        rec(&[4, b'a', b'b', b'c', b'd']).bytes(3),
        Err(ImportError::Limit(_))
    ));
    assert_eq!(
        msg(rec(&[3, b'a']).bytes(3).unwrap_err()),
        "the records end early"
    );
}
