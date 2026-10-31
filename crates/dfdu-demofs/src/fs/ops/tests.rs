use super::*;
use crate::Fault;
use crate::test_util::small;

#[test]
fn set_attr_changes_only_what_it_is_given() {
    let mut t = small();
    let iso = t.resolve("Downloads/a.iso").unwrap();
    let before = attr(&t, iso).unwrap();
    let same = set_attr(&mut t, iso, Changes::default(), 99).unwrap();
    assert_eq!(same, before);
    let c = Changes {
        mode: Some(0o100_600),
        uid: Some(5),
        gid: Some(6),
        ..Changes::default()
    };
    let a = set_attr(&mut t, iso, c, 99).unwrap();
    // The file type bits of the mode are not kept.
    assert_eq!((a.perm, a.uid, a.gid), (0o600, 5, 6));
    assert_eq!((a.size, a.mtime), (before.size, before.mtime));
}

#[test]
fn set_attr_sets_the_time_given_or_now() {
    let mut t = small();
    let iso = t.resolve("Downloads/a.iso").unwrap();
    let then = UNIX_EPOCH + Duration::from_secs(1_000);
    let c = Changes {
        mtime: Some(TimeOrNow::SpecificTime(then)),
        ..Changes::default()
    };
    assert_eq!(set_attr(&mut t, iso, c, 99).unwrap().mtime, then);
    assert_eq!(t.nodes[iso].mtime, 1_000);
    let c = Changes {
        mtime: Some(TimeOrNow::Now),
        ..Changes::default()
    };
    set_attr(&mut t, iso, c, 2_000).unwrap();
    assert_eq!(t.nodes[iso].mtime, 2_000);
    // A time before the epoch is kept as the epoch.
    let c = Changes {
        mtime: Some(TimeOrNow::SpecificTime(UNIX_EPOCH - Duration::from_secs(5))),
        ..Changes::default()
    };
    set_attr(&mut t, iso, c, 0).unwrap();
    assert_eq!(t.nodes[iso].mtime, 0);
}

#[test]
fn set_attr_truncates_first_and_stops_at_a_failure() {
    let mut t = small();
    let iso = t.resolve("Downloads/a.iso").unwrap();
    let c = Changes {
        size: Some(10),
        ..Changes::default()
    };
    assert_eq!(set_attr(&mut t, iso, c, 50).unwrap().size, 10);
    assert_eq!(t.nodes[iso].mtime, 50);
    let dl = t.resolve("Downloads").unwrap();
    let mode = t.nodes[dl].mode;
    let c = Changes {
        size: Some(0),
        mode: Some(0o700),
        ..Changes::default()
    };
    assert_eq!(set_attr(&mut t, dl, c, 50), Err(FsError::IsDir));
    assert_eq!(t.nodes[dl].mode, mode, "nothing changed after the failure");
    assert_eq!(
        set_attr(&mut t, 10_000, Changes::default(), 0),
        Err(FsError::NoEnt)
    );
}

#[test]
fn made_nodes_belong_to_who_made_them() {
    let mut t = small();
    let dl = t.resolve("Downloads").unwrap();
    let n = make(&mut t, dl, b"new", Kind::dir(), 77, 0o750, (501, 20)).unwrap();
    let i = &t.nodes[n];
    assert_eq!((i.uid, i.gid, i.mode, i.mtime), (501, 20, 0o750, 77));
    assert_eq!(t.resolve("Downloads/new"), Ok(n));
    assert_eq!(
        make(&mut t, dl, b"new", Kind::dir(), 77, 0o750, (501, 20)),
        Err(FsError::Exist)
    );
}

#[test]
fn only_symlinks_have_a_target() {
    let t = small();
    let link = t.resolve("dev/shop/link").unwrap();
    assert_eq!(link_target(&t, link), Ok(&b"package.json"[..]));
    let pkg = t.resolve("dev/shop/package.json").unwrap();
    assert_eq!(link_target(&t, pkg), Err(FsError::Inval));
    assert_eq!(link_target(&t, 10_000), Err(FsError::NoEnt));
}

#[test]
fn directories_open_after_their_delay_or_fail_with_their_fault() {
    let mut t = small();
    let dl = t.resolve("Downloads").unwrap();
    assert_eq!(open_delay(&t, dl), Ok(Duration::ZERO));
    let iso = t.resolve("Downloads/a.iso").unwrap();
    assert_eq!(open_delay(&t, iso), Err(FsError::NotDir));
    let locked = t.resolve("Library/locked").unwrap();
    assert_eq!(open_delay(&t, locked), Err(FsError::Denied));
    t.nodes[locked].fault = None;
    assert_eq!(open_delay(&t, locked), Ok(Duration::from_millis(250)));
    t.nodes[dl].fault = Some(Fault::Io);
    assert_eq!(open_delay(&t, dl), Err(FsError::Io));
    assert_eq!(open_delay(&t, 10_000), Err(FsError::NoEnt));
}
