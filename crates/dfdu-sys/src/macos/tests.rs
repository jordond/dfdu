use super::*;
use crate::bulk::{SF_FIRMLINK, VDIR, VLNK, VREG, parse_bulk_record};
use std::os::fd::AsFd;
use std::os::unix::fs::MetadataExt;

/// [`clone_info_at`] reads the same without opening the file, with its inode; a clone
/// rewritten in part keeps the rest shared. Skipped where the temporary directory is not on
/// APFS.
#[test]
fn clone_info_at_sees_partial_clones() {
    use std::io::{Seek, Write};
    let tmp = tempfile::tempdir().unwrap();
    if !shares_extents(tmp.path()).unwrap() {
        return;
    }
    let dir = std::fs::File::open(tmp.path()).unwrap();
    let a = tmp.path().join("a");
    // Synced, so the allocation is settled before it is compared (under load it was not).
    let mut f = std::fs::File::create(&a).unwrap();
    f.write_all(&vec![7u8; 1 << 20]).unwrap();
    f.sync_all().unwrap();
    drop(f);
    let st = std::fs::metadata(&a).unwrap();
    let at = |n: &std::ffi::CStr| clone_info_at(dir.as_fd(), n).unwrap();
    let alone = at(c"a");
    assert_eq!(alone.ino, st.ino());
    assert_eq!(alone.alloc, st.blocks() * 512);
    assert_eq!(alone.info.private_size, alone.alloc);
    let b = tmp.path().join("b");
    let ok = std::process::Command::new("cp")
        .arg("-c")
        .args([&a, &b])
        .status()
        .unwrap();
    assert!(ok.success());
    let mut f = std::fs::OpenOptions::new().write(true).open(&b).unwrap();
    f.seek(std::io::SeekFrom::Start(0)).unwrap();
    f.write_all(&vec![1u8; 65536]).unwrap();
    f.sync_all().unwrap();
    let part = at(c"b");
    assert_eq!(part.alloc, alone.alloc);
    assert!(part.info.private_size >= 65536 && part.info.private_size < part.alloc / 2);
    std::os::unix::fs::symlink("a", tmp.path().join("l")).unwrap();
    assert_ne!(
        clone_info_at(dir.as_fd(), c"l").ok().map(|c| c.ino),
        Some(st.ino())
    );
    assert!(clone_info_at(dir.as_fd(), c"missing").is_err());
}

/// `cp -c` makes a clone: it shares all its data, so neither copy has private bytes until
/// one is written to. Skipped where the temporary directory is not on APFS.
#[test]
fn clones_share_their_data() {
    let tmp = tempfile::tempdir().unwrap();
    let a = tmp.path().join("a");
    std::fs::write(&a, vec![7u8; 1 << 20]).unwrap();
    let open = |p: &std::path::Path| std::fs::File::open(p).unwrap();
    open(&a).sync_all().unwrap();
    let Some(alone) = clone_info(open(&a).as_fd()) else {
        return;
    };
    assert_eq!(alone.private_size, 1 << 20);
    let b = tmp.path().join("b");
    let ok = std::process::Command::new("cp")
        .arg("-c")
        .args([&a, &b])
        .status()
        .unwrap();
    assert!(ok.success());
    let (ia, ib) = (
        clone_info(open(&a).as_fd()).unwrap(),
        clone_info(open(&b).as_fd()).unwrap(),
    );
    assert_eq!((ia.private_size, ib.private_size), (0, 0));
    // Writing to a clone gives it private data of its own, once the write reaches the disk
    // (until then the private size can still be 0, seen under load).
    std::fs::OpenOptions::new()
        .write(true)
        .open(&b)
        .and_then(|mut f| {
            std::io::Write::write_all(&mut f, &[1u8; 4096])?;
            f.sync_all()
        })
        .unwrap();
    let ib = clone_info(open(&b).as_fd()).unwrap();
    assert!(ib.private_size >= 4096, "{ib:?}");
}

#[test]
fn records_match_lstat() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    std::fs::write(root.join("file"), vec![7u8; 10_000]).unwrap();
    std::fs::create_dir(root.join("dir")).unwrap();
    std::os::unix::fs::symlink("file", root.join("link")).unwrap();
    let dir = std::fs::File::open(root).unwrap();
    let mut buf = vec![0u8; 128 * 1024];
    let mut seen = Vec::new();
    loop {
        let n = read_bulk(dir.as_fd(), &mut buf).unwrap();
        if n == 0 {
            break;
        }
        let mut pos = 0;
        for _ in 0..n {
            let (e, next) = parse_bulk_record(&buf, pos).unwrap();
            pos = next;
            let name = std::str::from_utf8(e.name).unwrap().to_string();
            let md = std::fs::symlink_metadata(root.join(&name)).unwrap();
            assert_eq!(e.error, 0, "{name}");
            assert_eq!(
                e.dev.map(|d| i64::from(d).cast_unsigned()),
                Some(md.dev()),
                "{name}: dev"
            );
            assert_eq!(e.ino, Some(md.ino()), "{name}: ino");
            assert_eq!(e.mtime.map(|t| t.0), Some(md.mtime()), "{name}: mtime");
            assert_eq!(e.flags.unwrap() & SF_FIRMLINK, 0);
            let want = match name.as_str() {
                "dir" => VDIR,
                "link" => VLNK,
                _ => VREG,
            };
            assert_eq!(e.obj_type, Some(want), "{name}: type");
            if want != VDIR {
                assert_eq!(
                    e.nlink,
                    Some(u32::try_from(md.nlink()).unwrap()),
                    "{name}: nlink"
                );
                assert_eq!(
                    e.size,
                    Some(i64::try_from(md.size()).unwrap()),
                    "{name}: size"
                );
                let alloc = u64::try_from(e.alloc.unwrap()).unwrap();
                assert_eq!(alloc.div_ceil(512), md.blocks(), "{name}: blocks");
            }
            seen.push(name);
        }
    }
    seen.sort();
    assert_eq!(seen, ["dir", "file", "link"]);
}
