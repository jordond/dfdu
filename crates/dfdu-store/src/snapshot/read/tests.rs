use super::super::VERSION_3;
use super::super::tests::{bytes, header, sample};
use super::super::write::write_as;
use super::*;
use crate::ncdu::tests::dump;

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

/// A damaged frame is reported once, at the byte its frame starts, not re-wrapped by the
/// record reader.
#[test]
fn damage_names_its_frame_once() {
    let (t, r) = sample();
    let out = bytes(&t, r);
    let mut bad = out.clone();
    // The first frame's header is bytes 12..24; its compressed data follows.
    bad[24] ^= 0x41;
    let err = read(&bad[..], &Limits::default()).err().unwrap();
    assert_eq!(err.to_string(), "at byte 12: frame 0 is damaged");
    for i in 0..out.len() {
        let mut bad = out.clone();
        bad[i] ^= 0x41;
        let text = read(&bad[..], &Limits::default())
            .err()
            .unwrap()
            .to_string();
        assert!(text.matches("at byte").count() <= 1, "flip at {i}: {text}");
    }
}

/// Version 1 files (no journal position), version 2 files (no time) and version 3 files (no
/// last full scan) still read, with the same tree.
#[test]
fn older_versions_read() {
    let (t, r) = sample();
    let old = Header {
        last_full_scan: 0,
        scan_ms: 0,
        ..header()
    };
    let mut out = vec![];
    let v1 = Header {
        journal: None,
        ..old.clone()
    };
    write_as(&t, r, &mut out, &v1, VERSION_3).unwrap();
    out[8..12].copy_from_slice(&1u32.to_le_bytes());
    let l = read(&out[..], &Limits::default()).unwrap();
    assert_eq!(l.header, v1);
    assert_eq!(dump(&t, r), dump(&l.tree, l.root));
    let l = read(&bytes(&t, r)[..], &Limits::default()).unwrap();
    assert_eq!(l.header, header());
    // Version 2: a journal position without its time.
    let mut v2 = old.clone();
    v2.journal.as_mut().unwrap().time = 0;
    let mut out = vec![];
    write_as(&t, r, &mut out, &v2, VERSION_2).unwrap();
    let l = read(&out[..], &Limits::default()).unwrap();
    assert_eq!(l.header, v2);
    // Version 3: no last full scan, which is then taken to be when it was written.
    let mut out = vec![];
    write_as(&t, r, &mut out, &header(), VERSION_3).unwrap();
    assert_eq!(out[8..12], 3u32.to_le_bytes());
    let l = read(&out[..], &Limits::default()).unwrap();
    assert_eq!(l.header, old);
    assert_eq!(l.header.full_scan_at(), old.written);
    assert_eq!(header().full_scan_at(), header().last_full_scan);
    assert_eq!(dump(&t, r), dump(&l.tree, l.root));
}

/// The header alone reads from the start of a file, even one cut short after it.
#[test]
fn the_header_reads_alone() {
    let (t, r) = sample();
    let out = bytes(&t, r);
    let (root, h) = read_header(&out[..], &Limits::default()).unwrap();
    assert_eq!(root, t.path_buf(r).as_os_str().as_encoded_bytes());
    assert_eq!(h, header());
    assert!(read_header(&out[..12], &Limits::default()).is_err());
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
