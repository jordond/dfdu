use super::*;
use crate::test_support::{read_all, small};

#[test]
fn seeded_bytes_agree_at_any_offset() {
    let whole = seeded(42, 0, 100);
    assert_eq!(whole.len(), 100);
    for (start, end) in [(0, 8), (3, 5), (7, 9), (13, 100), (99, 100), (50, 50)] {
        assert_eq!(seeded(42, start as u64, end as u64), whole[start..end]);
    }
    assert_ne!(seeded(43, 0, 100), whole);
}

#[test]
fn seeds_and_the_mixer_are_fixed_functions() {
    // FNV-1a's offset basis, and its value for "a"; fixtures depend on these not changing.
    assert_eq!(seed_of(b""), 0xcbf2_9ce4_8422_2325);
    assert_eq!(seed_of(b"a"), 0xaf63_dc4c_8601_ec8c);
    assert_ne!(seed_of(b"ab"), seed_of(b"ba"));
    assert_eq!(mix(0), 0xe220_a839_7b1d_cdaf);
    assert_ne!(mix(1), mix(2));
}

#[test]
fn reads_stop_at_the_end_of_the_file() {
    let t = small();
    let a = t.resolve("Downloads/a.iso").unwrap();
    let whole = read_all(&t, a);
    assert_eq!(t.read(a, 13, 100).unwrap(), whole[13..113]);
    assert_eq!(t.read(a, (1 << 20) - 3, 100).unwrap().len(), 3);
    assert!(t.read(a, 1 << 20, 100).unwrap().is_empty());
    assert!(t.read(a, u64::MAX, u32::MAX).unwrap().is_empty());
    let dl = t.resolve("Downloads").unwrap();
    assert_eq!(t.read(dl, 0, 1), Err(FsError::IsDir));
}

#[test]
fn writes_and_truncates() {
    let mut t = small();
    let n = t
        .insert(0, b"note.trashinfo", Kind::stored(vec![]), 0, 0o644)
        .unwrap();
    t.write(n, 0, b"[Trash Info]\n", 7).unwrap();
    t.write(n, 13, b"Path=/x\n", 7).unwrap();
    assert_eq!(read_all(&t, n), b"[Trash Info]\nPath=/x\n");
    assert_eq!(t.nodes[n].mtime, 7);
    t.truncate(n, 5, 8).unwrap();
    assert_eq!(read_all(&t, n), b"[Tras");
    assert_eq!(t.nodes[n].mtime, 8);
    // Past the end leaves zeros between.
    t.write(n, 8, b"!", 9).unwrap();
    assert_eq!(read_all(&t, n), b"[Tras\0\0\0!");
    // A small generated file becomes stored bytes when written, keeping the rest.
    let a = t.resolve("Downloads/a.iso").unwrap();
    let before = read_all(&t, a);
    t.write(a, 0, b"XY", 9).unwrap();
    let after = read_all(&t, a);
    assert_eq!(&after[..2], b"XY");
    assert_eq!(after[2..], before[2..]);
    assert!(matches!(
        t.nodes[a].kind,
        Kind::File {
            data: Data::Bytes(_),
            ..
        }
    ));
    // A large one, or a write ending past the limit, is refused.
    let sparse = t.resolve("sparse.img").unwrap();
    assert_eq!(t.write(sparse, 0, b"x", 0), Err(FsError::TooBig));
    assert_eq!(t.write(n, MAX_STORED, b"x", 0), Err(FsError::TooBig));
    assert_eq!(t.write(n, u64::MAX, b"x", 0), Err(FsError::TooBig));
    let dl = t.resolve("Downloads").unwrap();
    assert_eq!(t.write(dl, 0, b"x", 0), Err(FsError::IsDir));
    assert_eq!(t.truncate(dl, 0, 0), Err(FsError::IsDir));
}

#[test]
fn a_change_drops_the_fixtures_allocation() {
    let mut t = small();
    let sparse = t.resolve("sparse.img").unwrap();
    t.truncate(sparse, 5 << 12, 0).unwrap();
    assert_eq!((t.size(sparse), t.alloc(sparse)), (5 << 12, 5 << 12));
    // Cut small enough, it can be written.
    t.write(sparse, 0, b"x", 0).unwrap();
    assert_eq!(read_all(&t, sparse).len(), 5 << 12);
}

#[test]
fn a_grown_file_reads_zeros_after_its_old_end() {
    let mut t = small();
    let a = t.resolve("Downloads/a.iso").unwrap();
    let head = t.read(a, 0, 64).unwrap();
    t.truncate(a, 32, 0).unwrap();
    t.truncate(a, 96, 0).unwrap();
    let b = read_all(&t, a);
    assert_eq!(b[..32], head[..32]);
    assert!(b[32..].iter().all(|&x| x == 0));
    // Stored bytes grow with zeros too.
    let pkg = t.resolve("dev/shop/package.json").unwrap();
    t.truncate(pkg, 4, 0).unwrap();
    assert_eq!(read_all(&t, pkg), b"{}\0\0");
    // Truncated to nothing, a generated file is stored and empty.
    t.truncate(a, 0, 0).unwrap();
    assert!(
        matches!(&t.nodes[a].kind, Kind::File { size: 0, data: Data::Bytes(b), .. } if b.is_empty())
    );
}
