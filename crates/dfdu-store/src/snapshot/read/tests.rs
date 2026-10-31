use super::super::tests::{bytes, header, sample};
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

/// The header and tree read back as written; a last full scan that is not known is taken to be
/// when the snapshot was written.
#[test]
fn a_snapshot_reads_back() {
    let (t, r) = sample();
    let l = read(&bytes(&t, r)[..], &Limits::default()).unwrap();
    assert_eq!(l.header, header());
    assert_eq!(dump(&t, r), dump(&l.tree, l.root));
    assert_eq!(header().full_scan_at(), header().last_full_scan);
    let unknown = Header {
        last_full_scan: 0,
        ..header()
    };
    assert_eq!(unknown.full_scan_at(), unknown.written);
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

/// A format version other than this build's is refused, older ones included, and so is a file
/// without the magic.
#[test]
fn other_versions_and_other_files_are_refused() {
    let (t, r) = sample();
    for version in [1, 2, 3, VERSION + 1] {
        let mut out = bytes(&t, r);
        out[8..12].copy_from_slice(&u32::to_le_bytes(version));
        let e = read(&out[..], &Limits::default()).err().unwrap();
        assert_eq!(
            e.to_string(),
            format!("at byte 8: snapshot format {version}; this dfdu reads {VERSION}")
        );
        assert!(read_header(&out[..], &Limits::default()).is_err());
    }
    assert!(!is_snapshot(b"[1,2,{}"));
    assert!(read(&b"[1,2,{},[{\"name\":\"/\"}]]"[..], &Limits::default()).is_err());
}
