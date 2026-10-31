use dfdu_tree::{Kind, Meta};

use super::*;

fn meta(kind: Kind, ino: u64) -> Meta {
    Meta {
        kind,
        apparent: 10,
        alloc: 4096,
        mtime: -3,
        dev: 1,
        ino,
        nlink: 1,
        mount: 0,
    }
}

/// A chunk of a directory named `names`, with nothing in it.
fn empty(names: &[u8]) -> Chunk {
    Chunk {
        job: JobId(1),
        dir: DirId(2),
        seq: 3,
        root_meta: None,
        probe: None,
        names: names.to_vec(),
        files: Vec::new(),
        first_subdir: DirId(4),
        subdirs: Vec::new(),
        errors: Vec::new(),
        marks: Vec::new(),
        status: ChunkStatus::Cancelled,
    }
}

fn encode(c: &Chunk) -> Vec<u8> {
    let mut e = Enc::default();
    enc_chunk(&mut e, c);
    e.0
}

fn decode(buf: &[u8]) -> R<Chunk> {
    let mut d = Dec::new(buf);
    let c = dec_chunk(&mut d, None)?;
    d.end()?;
    Ok(c)
}

/// The reason `buf` does not decode.
fn malformed(buf: &[u8]) -> &'static str {
    match decode(buf) {
        Err(WireError::Malformed(why)) => why,
        other => panic!("{other:?}"),
    }
}

/// A chunk comes back as it went, a probe without a subvolume or a profile included.
#[test]
fn a_chunk_comes_back_as_it_went() {
    let mut c = empty(b"fsub");
    c.root_meta = Some(meta(Kind::Dir, 1));
    c.probe = Some(Box::new(Probe {
        subvol: None,
        shares_extents: false,
        profile: None,
    }));
    c.files.push(FileRec {
        name: NameSpan { off: 0, len: 1 },
        meta: meta(Kind::File, 2),
    });
    c.subdirs.push(SubdirRec {
        name: NameSpan { off: 1, len: 3 },
        meta: meta(Kind::Dir, 3),
        mount_root: false,
        skip: Some(SkipReason::Kept),
    });
    c.marks.push((Entry::Subdir(0), Mark::Protected));
    let back = decode(&encode(&c)).unwrap();
    assert_eq!(format!("{back:?}"), format!("{c:?}"));
    let c = empty(b"");
    assert_eq!(
        format!("{:?}", decode(&encode(&c)).unwrap()),
        format!("{c:?}")
    );
}

/// No prefix of a chunk decodes: every field is read against what is left.
#[test]
fn a_cut_chunk_does_not_decode() {
    let mut c = empty(b"name");
    c.files.push(FileRec {
        name: NameSpan { off: 0, len: 4 },
        meta: meta(Kind::File, 2),
    });
    c.status = ChunkStatus::Panicked("boom".into());
    let buf = encode(&c);
    for n in 0..buf.len() {
        assert!(decode(&buf[..n]).is_err(), "{n} of {} bytes", buf.len());
    }
}

/// A name outside the chunk's names, a mark of an entry it does not have, and subdirectory
/// ids past the last are refused.
#[test]
fn what_points_outside_the_chunk_is_refused() {
    let mut c = empty(b"ab");
    c.files.push(FileRec {
        name: NameSpan { off: 1, len: 2 },
        meta: meta(Kind::File, 2),
    });
    assert_eq!(malformed(&encode(&c)), "name outside the chunk's names");

    let mut c = empty(b"ab");
    c.files.push(FileRec {
        name: NameSpan { off: 0, len: 2 },
        meta: meta(Kind::File, 2),
    });
    c.marks.push((Entry::File(1), Mark::Excepted));
    assert_eq!(malformed(&encode(&c)), "mark of an entry not in the chunk");
    c.marks = vec![(Entry::Subdir(0), Mark::Excepted)];
    assert_eq!(malformed(&encode(&c)), "mark of an entry not in the chunk");

    let mut c = empty(b"ab");
    c.first_subdir = DirId(u32::MAX);
    c.subdirs.push(SubdirRec {
        name: NameSpan { off: 0, len: 2 },
        meta: meta(Kind::Dir, 3),
        mount_root: false,
        skip: None,
    });
    assert_eq!(malformed(&encode(&c)), "subdirectory ids overflow");
}

/// A profile's data ratio must be a positive number.
#[test]
fn a_probe_with_a_bad_ratio_is_refused() {
    for ratio in [0.0, -1.0, f64::NAN, f64::INFINITY] {
        let mut c = empty(b"");
        c.probe = Some(Box::new(Probe {
            subvol: None,
            shares_extents: true,
            profile: Some((dfdu_sys::Profile::Raid1, ratio)),
        }));
        assert_eq!(malformed(&encode(&c)), "profile ratio", "{ratio}");
    }
}

/// An unknown tag byte is refused, as the status of a chunk shows.
#[test]
fn an_unknown_tag_is_refused() {
    let mut buf = encode(&empty(b""));
    *buf.last_mut().unwrap() = 9;
    assert_eq!(malformed(&buf), "chunk status");
}

#[test]
fn an_entry_is_in_the_chunk_when_its_index_is_below_the_count() {
    assert!(in_chunk(Entry::File(0), 1).is_ok());
    assert!(in_chunk(Entry::Subdir(1), 1).is_err());
}
