use dfdu_tree::Meta;

use super::*;

fn meta(kind: Kind, size: u64, dev: u64, ino: u64, nlink: u32) -> Meta {
    Meta {
        kind,
        apparent: size,
        alloc: size,
        mtime: 0,
        dev,
        ino,
        nlink,
        mount: 0,
    }
}

fn set_state(t: &mut Tree, ix: Ix, state: DirState) {
    t.dir_mut(ix).unwrap().state = state;
}

fn exported(t: &Tree, root: Ix, meta: &Metadata) -> (Vec<u8>, ExportStats) {
    let mut out = vec![];
    let stats = export(t, root, &mut out, meta).unwrap();
    (out, stats)
}

/// Skipped directories say why and are not opened, a directory that was not read to the end
/// has a read error, `nlink` is the number of links in the tree (at least two), and `dev` is
/// written only where it changes.
#[test]
fn info_blocks_carry_what_ncdu_reads() {
    let mut t = Tree::new();
    let root = t.new_root(b"/r", &meta(Kind::Dir, 0, 7, 1, 1), true);
    set_state(&mut t, root, DirState::Complete);
    let dir = |t: &mut Tree, name: &[u8], dev: u64, flag: u16, state: DirState| {
        let ix = t.add_dir_flagged(root, name, &meta(Kind::Dir, 0, dev, 2, 1), flag);
        set_state(t, ix, state);
    };
    dir(&mut t, b"proc", 7, NodeFlags::KERNEL, DirState::Skipped);
    dir(&mut t, b"mnt", 9, NodeFlags::MOUNT_ROOT, DirState::Skipped);
    dir(&mut t, b"skip", 7, 0, DirState::Skipped);
    dir(&mut t, b"bad", 7, 0, DirState::Failed);
    t.add_file(root, b"h1", &meta(Kind::File, 3, 7, 5, 9), true, None);
    t.add_file(root, b"h2", &meta(Kind::File, 3, 7, 5, 9), false, None);
    t.add_file(root, b"h3", &meta(Kind::File, 3, 7, 5, 9), false, None);
    t.add_file(root, b"one", &meta(Kind::File, 1, 7, 6, 4), true, None);
    t.add_file(root, b"l", &meta(Kind::Symlink, 1, 7, 8, 1), false, None);
    let (got, stats) = exported(&t, root, &Metadata::default());
    let want = br#"[1,2,{},
[{"name":"/r","dev":7},
{"name":"proc","excluded":"kernfs"},
{"name":"mnt","excluded":"otherfs","dev":9},
{"name":"skip","excluded":"pattern"},
[{"name":"bad","read_error":true}],
{"name":"h1","asize":3,"dsize":3,"ino":5,"hlnkc":true,"nlink":3},
{"name":"h2","asize":3,"dsize":3,"ino":5,"hlnkc":true,"nlink":3},
{"name":"h3","asize":3,"dsize":3,"ino":5,"hlnkc":true,"nlink":3},
{"name":"one","asize":1,"dsize":1,"ino":6,"hlnkc":true,"nlink":2},
{"name":"l","asize":1,"dsize":1,"notreg":true}]]
"#;
    assert_eq!(
        got.escape_ascii().to_string(),
        want.escape_ascii().to_string()
    );
    assert_eq!(
        stats,
        ExportStats {
            entries: 10,
            non_utf8: 0
        }
    );
}

/// The header holds only the fields given; an unfinished root has a read error, and a
/// negative modification time is left out.
#[test]
fn the_header_and_the_root_carry_what_is_known() {
    let mut t = Tree::new();
    let m = Meta {
        mtime: -5,
        ..meta(Kind::Dir, 4096, 1, 1, 1)
    };
    let root = t.new_root(b"/r", &m, true);
    let f = t.add_file(
        root,
        b"\xff",
        &Meta {
            mtime: 1_700_000_000,
            ..meta(Kind::File, 1, 1, 2, 1)
        },
        false,
        Some(dfdu_tree::ErrKind::NotFound),
    );
    assert!(t.error(f).is_some());
    let meta = Metadata {
        progname: None,
        progver: Some("9".into()),
        timestamp: Some(12),
    };
    let (got, stats) = exported(&t, root, &meta);
    let want = b"[1,2,{\"progver\":\"9\",\"timestamp\":12},\n\
        [{\"name\":\"/r\",\"asize\":4096,\"dsize\":4096,\"dev\":1,\"read_error\":true},\n\
        {\"name\":\"\xff\",\"asize\":1,\"dsize\":1,\"read_error\":true,\"mtime\":1700000000}]]\n";
    assert_eq!(
        got.escape_ascii().to_string(),
        want.escape_ascii().to_string()
    );
    assert_eq!(stats.non_utf8, 1);
}
