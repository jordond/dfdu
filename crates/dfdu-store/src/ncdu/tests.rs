use super::*;

/// What ncdu 2.9 wrote for a small tree (`ncdu -o-`): a hard link in two directories, a
/// symlink, a directory excluded by pattern, and one on another device.
pub(crate) const NCDU: &[u8] =
    br#"[1,2,{"progname":"ncdu","progver":"2.9.2","timestamp":1793154464},
[{"name":"/tmp/nt","asize":160,"dev":16777234},
{"name":"h","asize":3,"dsize":4096,"ino":418454134,"hlnkc":true,"nlink":2},
{"name":"foo","excluded":"pattern"},
{"name":"proc","excluded":"kernfs"},
[{"name":"bar","asize":128},
{"name":"f","asize":3,"dsize":4096,"ino":418454134,"hlnkc":true,"nlink":2},
{"name":"l","asize":1,"notreg":true}],
[{"name":"mnt","asize":64,"dsize":4096,"dev":99},
{"name":"big","asize":10000,"dsize":12288,"mtime":1700000000.25}],
[{"name":"unreadable","read_error":true}]]]
"#;

fn import_bytes(b: &[u8]) -> Imported {
    import(b, &Limits::default()).unwrap()
}

fn find(t: &Tree, root: Ix, path: &str) -> Ix {
    let mut ix = root;
    for n in path.split('/') {
        ix = t
            .children(ix)
            .find(|&c| t.name(c) == n.as_bytes())
            .unwrap_or_else(|| panic!("{path}"));
    }
    ix
}

/// Every entry below `root`, one line each: path, kind, own sizes, totals, state, flags.
pub(crate) fn dump(t: &Tree, root: Ix) -> Vec<String> {
    let mut out = vec![];
    let mut stack = vec![(root, String::new())];
    while let Some((ix, path)) = stack.pop() {
        let tot = t.total(ix);
        let f = t.flags(ix);
        out.push(format!(
            "{path} {:?} {:?} total={}/{}/{} err={} state={:?} mount={} kernel={} link={} charged={} mtime={}",
            t.kind(ix),
            t.raw_size(ix),
            tot.apparent,
            tot.alloc,
            tot.items,
            tot.errors,
            t.dir(ix).map(|d| d.state),
            f.has(NodeFlags::MOUNT_ROOT),
            f.has(NodeFlags::KERNEL),
            f.has(NodeFlags::HARDLINK),
            f.has(NodeFlags::CHARGED),
            t.mtime(ix),
        ));
        let mut kids: Vec<Ix> = t.children(ix).collect();
        kids.sort_by_key(|&c| t.name(c).to_vec());
        for c in kids.into_iter().rev() {
            let n = String::from_utf8_lossy(t.name(c)).into_owned();
            stack.push((c, format!("{path}/{n}")));
        }
    }
    out
}

#[test]
fn imports_what_ncdu_writes() {
    let im = import_bytes(NCDU);
    let (t, root) = (&im.tree, im.root);
    t.check(&[]).unwrap();
    assert_eq!(im.minor, 2);
    assert_eq!(im.meta.progname.as_deref(), Some("ncdu"));
    assert_eq!(im.meta.timestamp, Some(1793154464));
    assert_eq!(t.name(root), b"/tmp/nt");
    assert_eq!(im.entries, 10);
    // The hard link counts once.
    let tot = t.total(root);
    assert_eq!(tot.alloc, 4096 + 4096 + 12288);
    assert_eq!(tot.apparent, 160 + 3 + 128 + 1 + 64 + 10000);
    assert_eq!(tot.items, 10);
    assert_eq!(tot.errors, 1);
    assert_eq!(tot.mounts, 1);
    let h = find(t, root, "h");
    let f = find(t, root, "bar/f");
    assert!(t.flags(h).has(NodeFlags::HARDLINK) && t.flags(f).has(NodeFlags::HARDLINK));
    assert!(t.flags(h).has(NodeFlags::CHARGED) != t.flags(f).has(NodeFlags::CHARGED));
    assert_eq!(t.kind(find(t, root, "bar/l")), Kind::Other);
    let foo = find(t, root, "foo");
    assert_eq!(t.dir(foo).unwrap().state, DirState::Skipped);
    assert!(t.flags(find(t, root, "proc")).has(NodeFlags::KERNEL));
    let mnt = find(t, root, "mnt");
    assert!(t.flags(mnt).has(NodeFlags::MOUNT_ROOT));
    assert_eq!(t.dev_ino(find(t, root, "mnt/big")).0, 99);
    assert_eq!(t.mtime(find(t, root, "mnt/big")), 1_700_000_000);
    let u = find(t, root, "unreadable");
    assert_eq!(t.dir(u).unwrap().state, DirState::Partial);
    assert_eq!(t.dir(root).unwrap().state, DirState::Complete);
}

#[test]
fn round_trip_keeps_the_tree() {
    let im = import_bytes(NCDU);
    let mut out = vec![];
    let meta = Metadata {
        progname: Some("dfdu".into()),
        progver: Some("0.1.0".into()),
        timestamp: Some(5),
    };
    let stats = export(&im.tree, im.root, &mut out, &meta).unwrap();
    assert_eq!(
        stats,
        ExportStats {
            entries: 10,
            non_utf8: 0
        }
    );
    let again = import_bytes(&out);
    assert_eq!(again.meta, meta);
    assert_eq!(dump(&im.tree, im.root), dump(&again.tree, again.root));
    // And the export of the export is the same bytes.
    let mut out2 = vec![];
    export(&again.tree, again.root, &mut out2, &meta).unwrap();
    assert_eq!(
        String::from_utf8_lossy(&out),
        String::from_utf8_lossy(&out2)
    );
}

#[test]
fn names_that_are_not_utf8_survive() {
    let src = b"[1,2,{},[{\"name\":\"/r\\u00e9\"},{\"name\":\"a\xff\\\"b\",\"asize\":1},{\"name\":\"\\ud83d\\ude00\"}]]";
    let im = import_bytes(src);
    assert_eq!(im.tree.name(im.root), "/ré".as_bytes());
    let names: Vec<Vec<u8>> = im
        .tree
        .children(im.root)
        .map(|c| im.tree.name(c).to_vec())
        .collect();
    assert_eq!(names, [b"a\xff\"b".to_vec(), "😀".as_bytes().to_vec()]);
    let mut out = vec![];
    let stats = export(&im.tree, im.root, &mut out, &Metadata::default()).unwrap();
    assert_eq!(stats.non_utf8, 1);
    let again = import_bytes(&out);
    assert_eq!(dump(&im.tree, im.root), dump(&again.tree, again.root));
}

#[test]
fn compressed_input() {
    let mut w = crate::ZstdWriter::new(vec![]);
    // Several frames.
    for chunk in NCDU.chunks(100) {
        std::io::Write::write_all(&mut w, chunk).unwrap();
        w.frame().unwrap();
    }
    let z = w.finish().unwrap();
    let im = import(&z[..], &Limits::default()).unwrap();
    assert_eq!(
        dump(&im.tree, im.root),
        dump(&import_bytes(NCDU).tree, import_bytes(NCDU).root)
    );
}

#[test]
fn zstd_flush_reaches_the_sink() {
    use std::io::{Read, Write};
    let mut w = crate::ZstdWriter::new(vec![]);
    w.write_all(b"first ").unwrap();
    w.flush().unwrap();
    assert!(w.buf.is_empty());
    let flushed = w.inner.len();
    assert!(flushed > 0);
    w.write_all(b"second").unwrap();
    let z = w.finish().unwrap();
    assert!(z.len() > flushed);
    let mut out = String::new();
    crate::ZstdReader::new(&z[..])
        .read_to_string(&mut out)
        .unwrap();
    assert_eq!(out, "first second");
}

#[test]
fn bad_input_is_rejected() {
    let bad: &[&[u8]] = &[
        b"",
        b"[2,0,{},[{\"name\":\"/\"}]]",
        b"[1,2,{},[{\"name\":\"/\"}]",
        b"[1,2,{},[{\"name\":\"/\"},{\"name\":\"..\"}]]",
        b"[1,2,{},[{\"name\":\"/\"},{\"name\":\"a/b\"}]]",
        b"[1,2,{},[{\"name\":\"/\"},{\"name\":\"\"}]]",
        b"[1,2,{},[{\"name\":\"/\"},{\"asize\":1}]]",
        b"[1,2,{},[{\"name\":\"/\"},{\"name\":\"a\",\"asize\":-1}]]",
        b"[1,2,{},[{\"name\":\"/\"},7]]",
        b"[1,2,{},[{\"name\":\"/\"}]] x",
        b"[1,2,{},[{\"name\":\"/\"},[{\"name\":\"a\"}]]",
    ];
    for b in bad {
        assert!(
            import(*b, &Limits::default()).is_err(),
            "{}",
            b.escape_ascii()
        );
    }
}

#[test]
fn limits_hold() {
    let deep = |n: usize| {
        let mut s = b"[1,2,{},[{\"name\":\"/\"}".to_vec();
        for _ in 0..n {
            s.extend_from_slice(b",[{\"name\":\"d\"}");
        }
        s.extend(std::iter::repeat_n(b']', n + 2));
        s
    };
    let limits = Limits {
        max_depth: 10,
        max_name: 8,
        max_entries: 20,
    };
    assert!(import(&deep(10)[..], &limits).is_ok());
    assert!(matches!(
        import(&deep(11)[..], &limits),
        Err(ImportError::Limit(_))
    ));
    let wide = |n: usize| {
        let mut s = b"[1,2,{},[{\"name\":\"/\"}".to_vec();
        for i in 0..n {
            s.extend_from_slice(format!(",{{\"name\":\"f{i}\"}}").as_bytes());
        }
        s.extend_from_slice(b"]]");
        s
    };
    assert!(import(&wide(19)[..], &limits).is_ok());
    assert!(matches!(
        import(&wide(20)[..], &limits),
        Err(ImportError::Limit(_))
    ));
    let long = b"[1,2,{},[{\"name\":\"/\"},{\"name\":\"123456789\"}]]";
    assert!(matches!(
        import(&long[..], &limits),
        Err(ImportError::Limit(_))
    ));
    // A million levels do not overflow the stack with the default limits: they stop at the
    // depth limit.
    assert!(matches!(
        import(&deep(1_000_000)[..], &Limits::default()),
        Err(ImportError::Limit(_))
    ));
}
