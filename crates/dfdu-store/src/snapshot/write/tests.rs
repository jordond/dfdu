use std::path::Path;

use super::super::VERSION_2;
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
    let mut fw = FrameWriter {
        w: vec![],
        raw: vec![],
        frames: 0,
        crcs: vec![],
    };
    fw.frame().unwrap();
    assert_eq!((fw.w.len(), fw.frames), (0, 0));
    fw.raw.extend_from_slice(b"records");
    fw.maybe_frame().unwrap();
    assert_eq!(
        fw.frames, 0,
        "below a frame's worth, nothing is written yet"
    );
    fw.frame().unwrap();
    assert_eq!(fw.frames, 1);
    assert!(fw.raw.is_empty());
    assert_eq!(fw.w[4..8], 7u32.to_le_bytes(), "the raw length");
}

/// Format 2 leaves out the journal position's time; without a position neither has more.
#[test]
fn the_header_drops_the_journal_time_in_format_2() {
    let h = header();
    let (mut v3, mut v2) = (vec![], vec![]);
    put_header(&mut v3, Path::new("/r"), &h, VERSION);
    put_header(&mut v2, Path::new("/r"), &h, VERSION_2);
    let time = u(h.journal.unwrap().time);
    assert_eq!(v3, [&v2[..], &time[..]].concat());
    let none = Header {
        journal: None,
        ..header()
    };
    let (mut a, mut b) = (vec![], vec![]);
    put_header(&mut a, Path::new("/r"), &none, VERSION);
    put_header(&mut b, Path::new("/r"), &none, VERSION_2);
    assert_eq!(a, b);
    assert!(a.len() < v2.len());
}
