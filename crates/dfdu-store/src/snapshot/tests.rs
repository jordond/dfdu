use super::*;
use crate::ncdu::tests::{NCDU, dump};

fn sample() -> (Tree, Ix) {
    let im = crate::ncdu::import(NCDU, &Limits::default()).unwrap();
    (im.tree, im.root)
}

fn header() -> Header {
    Header {
        tool_version: "0.1.0".into(),
        written: 1_793_000_000,
        cross_mounts: true,
        network: false,
    }
}

fn bytes(t: &Tree, r: Ix) -> Vec<u8> {
    let mut out = vec![];
    write(t, r, &mut out, &header()).unwrap();
    out
}

#[test]
fn round_trip() {
    let (t, r) = sample();
    let out = bytes(&t, r);
    assert!(is_snapshot(&out));
    let l = read(&out[..], &Limits::default()).unwrap();
    l.tree.check(&[]).unwrap();
    assert_eq!(l.header, header());
    assert_eq!(l.entries, 10);
    assert_eq!(dump(&t, r), dump(&l.tree, l.root));
    assert_eq!(bytes(&l.tree, l.root), out);
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

/// A tree large enough for several frames, with names that are not UTF-8.
#[test]
fn many_frames() {
    let mut src = b"[1,2,{},[{\"name\":\"/big\"}".to_vec();
    for d in 0..40 {
        src.extend_from_slice(format!(",[{{\"name\":\"d{d}\"}}").as_bytes());
        for f in 0..2000 {
            src.extend_from_slice(
                format!(",{{\"name\":\"file-\u{e9}-{f}-\\u00ff\",\"asize\":{f},\"dsize\":4096}}")
                    .as_bytes(),
            );
        }
        src.push(b']');
    }
    src.extend_from_slice(b"]]");
    let im = crate::ncdu::import(&src[..], &Limits::default()).unwrap();
    let out = bytes(&im.tree, im.root);
    let l = read(&out[..], &Limits::default()).unwrap();
    assert_eq!(dump(&im.tree, im.root), dump(&l.tree, l.root));
    let frames = u64::from_le_bytes(out[out.len() - 16..out.len() - 8].try_into().unwrap());
    assert!(frames > 1, "{frames}");
    let small = Limits {
        max_entries: 1000,
        ..Limits::default()
    };
    assert!(matches!(read(&out[..], &small), Err(ImportError::Limit(_))));
}
