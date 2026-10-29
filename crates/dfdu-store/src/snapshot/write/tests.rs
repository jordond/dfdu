use std::path::Path;

use super::super::{VERSION_2, VERSION_3};
use super::super::tests::{header, sample};
use super::*;

fn u(v: u64) -> Vec<u8> {
    let mut out = vec![];
    put_u(&mut out, v);
    out
}

fn i(v: i64) -> Vec<u8> {
    let mut out = vec![];
    put_i(&mut out, v);
    out
}

#[test]
fn varints_are_little_end_first_and_signed_ones_zigzagged() {
    assert_eq!(u(0), [0]);
    assert_eq!(u(127), [0x7f]);
    assert_eq!(u(128), [0x80, 0x01]);
    assert_eq!(u(300), [0xac, 0x02]);
    let max = u(u64::MAX);
    assert_eq!(max.len(), 10);
    assert_eq!(max[9], 0x01);
    assert_eq!((i(0), i(-1), i(1), i(-2)), (u(0), u(1), u(2), u(3)));
    assert_eq!(i(i64::MIN), max);
    assert_eq!(i(i64::MAX), u(u64::MAX - 1));
    let mut b = vec![];
    put_bytes(&mut b, b"abc");
    assert_eq!(b, [3, b'a', b'b', b'c']);
}

/// The file is the magic, the version, frames, a header of zeros and a footer that counts the
/// entries and frames and checks itself.
#[test]
fn the_footer_counts_and_checks_what_was_written() {
    let (t, r) = sample();
    let mut out = vec![];
    let entries = write(&t, r, &mut out, &header()).unwrap();
    assert_eq!(entries, 10);
    assert_eq!(&out[..8], MAGIC);
    assert_eq!(out[8..12], VERSION.to_le_bytes());
    let foot = &out[out.len() - 24..];
    assert_eq!(foot[..8], entries.to_le_bytes());
    assert_eq!(
        foot[8..16],
        1u64.to_le_bytes(),
        "a small tree takes one frame"
    );
    assert_eq!(foot[20..], crc32c::crc32c(&foot[..20]).to_le_bytes());
    assert_eq!(out[out.len() - 36..out.len() - 24], [0; 12]);
    // The one frame's CRC is the footer's CRC of frame CRCs' only input.
    let frame_crc = &out[20..24];
    assert_eq!(foot[16..20], crc32c::crc32c(frame_crc).to_le_bytes());
}

#[test]
fn an_empty_frame_is_not_written() {
    let mut frames = Frames::default();
    let mut w = vec![];
    frames.frame(&mut w, &mut vec![]).unwrap();
    assert_eq!((w.len(), frames.frames), (0, 0));
    let mut raw = b"records".to_vec();
    frames.frame(&mut w, &mut raw).unwrap();
    assert_eq!(frames.frames, 1);
    assert_eq!(raw, Vec::<u8>::new());
    assert_eq!(w[4..8], 7u32.to_le_bytes(), "the raw length");
}

/// Written a few entries at a time, flushed between the steps, a snapshot is the same bytes as
/// written at once; below a frame's worth a flush writes no frame.
#[test]
fn a_snapshot_written_in_steps_is_the_same() {
    let (t, r) = sample();
    let mut whole = vec![];
    write(&t, r, &mut whole, &header()).unwrap();
    let mut w = Writer::new(&t, r, &header());
    let mut out = vec![];
    let mut steps = 0;
    while !w.step(&t, 3) {
        w.flush(&mut out).unwrap();
        assert_eq!(out.len(), 12, "only the magic and version");
        steps += 1;
    }
    assert_eq!(steps, 3);
    assert_eq!(w.entries(), 10);
    assert_eq!(w.finish(&mut out).unwrap(), 10);
    assert_eq!(out, whole);
    let early = Writer::new(&t, r, &header());
    assert!(early.finish(&mut vec![]).is_err());
}

/// Format 2 leaves out the journal position's time, format 3 the last full scan; without a
/// position formats 2 and 3 have nothing more.
#[test]
fn the_header_drops_newer_fields_in_older_formats() {
    let h = header();
    let (mut v4, mut v3, mut v2) = (vec![], vec![], vec![]);
    put_header(&mut v4, Path::new("/r"), &h, VERSION);
    put_header(&mut v3, Path::new("/r"), &h, VERSION_3);
    put_header(&mut v2, Path::new("/r"), &h, VERSION_2);
    let time = u(h.journal.unwrap().time);
    assert_eq!(v3, [&v2[..], &time[..]].concat());
    let full = [u(h.last_full_scan), u(h.scan_ms)].concat();
    assert_eq!(v4, [&v3[..], &full[..]].concat());
    let none = Header {
        journal: None,
        ..header()
    };
    let (mut a, mut b) = (vec![], vec![]);
    put_header(&mut a, Path::new("/r"), &none, VERSION_3);
    put_header(&mut b, Path::new("/r"), &none, VERSION_2);
    assert_eq!(a, b);
    assert!(a.len() < v2.len());
}
