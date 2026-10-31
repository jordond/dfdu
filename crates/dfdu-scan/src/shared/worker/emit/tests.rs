use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::PathBuf;

use super::*;
use crate::proto::{JobId, ListingStatus};
use crate::shared::test_util::{Inbox, Local, close_of, run, task};
use crate::shared::{JobOptions, Limits};

/// A directory on device `dev` and mount `mount`, with inode `ino`.
fn dir_meta(dev: u64, mount: u64, ino: u64) -> Meta {
    Meta {
        kind: Kind::Dir,
        apparent: 0,
        alloc: 0,
        mtime: 0,
        dev,
        ino,
        nlink: 1,
        mount,
    }
}

const KERNEL: u64 = 10;
const NETWORK: u64 = 11;
const DISK: u64 = 12;

fn shared() -> Shared<Local> {
    let classes = HashMap::from([
        (KERNEL, FsClass::Kernel),
        (NETWORK, FsClass::Network),
        (DISK, FsClass::Local),
    ]);
    Shared::new(Local::with_classes(classes), Limits::default())
}

#[test]
fn subdirectories_of_the_same_mount_are_read_when_they_can_be() {
    let sh = shared();
    let t = task(DirId(1), PathBuf::new(), 1, 1);
    let j = JobState::new(JobOptions::default(), None);
    let m = dir_meta(1, 1, 5);
    assert_eq!(subdir_skip(&sh, &t, &j, &m, true), (false, None));
    let unreadable = Some(SkipReason::Excluded);
    assert_eq!(subdir_skip(&sh, &t, &j, &m, false), (false, unreadable));
    // Without mount ids, a change of device is a mount root.
    let t = task(DirId(1), PathBuf::new(), 1, 0);
    let other = Some(SkipReason::OtherFilesystem);
    assert_eq!(
        subdir_skip(&sh, &t, &j, &dir_meta(2, 0, 5), true),
        (true, other)
    );
}

/// A directory a resume keeps is not read, the first time it is met, unless its mtime changed;
/// a mount root never is.
#[test]
fn kept_directories_are_not_read() {
    let sh = shared();
    let t = task(DirId(1), PathBuf::new(), 1, 1);
    let keep = crate::journal::Keep::new([((1, 5), 0), ((2, 6), 0), ((1, 7), 0)]);
    let j = JobState::new(
        JobOptions {
            keep: Some(Arc::new(keep)),
            ..JobOptions::default()
        },
        None,
    );
    let m = dir_meta(1, 1, 5);
    let kept = Some(SkipReason::Kept);
    assert_eq!(subdir_skip(&sh, &t, &j, &m, true), (false, kept));
    assert_eq!(subdir_skip(&sh, &t, &j, &m, true), (false, None));
    let changed = Meta {
        mtime: 1,
        ..dir_meta(1, 1, 7)
    };
    assert_eq!(subdir_skip(&sh, &t, &j, &changed, true), (false, None));
    let other = Some(SkipReason::OtherFilesystem);
    assert_eq!(
        subdir_skip(&sh, &t, &j, &dir_meta(2, 2, 6), true),
        (true, other)
    );
}

#[test]
fn mounts_are_read_as_the_job_options_say() {
    let sh = shared();
    let t = task(DirId(1), PathBuf::new(), 1, 1);
    let skip = |opts: JobOptions, mount: u64| {
        let j = JobState::new(opts, None);
        subdir_skip(&sh, &t, &j, &dir_meta(2, mount, 5), true).1
    };
    let cross = JobOptions {
        cross_mounts: true,
        ..JobOptions::default()
    };
    let network = JobOptions {
        network: true,
        ..JobOptions::default()
    };
    let entered = JobOptions {
        enter: vec![(2, 5)],
        ..JobOptions::default()
    };
    assert_eq!(
        skip(JobOptions::default(), DISK),
        Some(SkipReason::OtherFilesystem)
    );
    assert_eq!(skip(cross.clone(), DISK), None);
    assert_eq!(skip(cross.clone(), NETWORK), Some(SkipReason::Network));
    assert_eq!(skip(network, NETWORK), None);
    assert_eq!(skip(entered.clone(), DISK), None);
    assert_eq!(skip(entered, NETWORK), None);
    // Kernel filesystems never, entered or not.
    let all = JobOptions {
        cross_mounts: true,
        network: true,
        enter: vec![(2, 5)],
        ..JobOptions::default()
    };
    assert_eq!(skip(all, KERNEL), Some(SkipReason::Kernel));
}

#[test]
fn a_mount_seen_again_is_a_duplicate() {
    let sh = shared();
    let t = task(DirId(1), PathBuf::new(), 1, 1);
    let cross = JobOptions {
        cross_mounts: true,
        ..JobOptions::default()
    };
    // Without a mount table, by the device and inode of its root.
    let j = JobState::new(cross.clone(), None);
    let m = dir_meta(2, DISK, 5);
    assert_eq!(subdir_skip(&sh, &t, &j, &m, true), (true, None));
    let dup = Some(SkipReason::DuplicateMount);
    assert_eq!(subdir_skip(&sh, &t, &j, &m, true), (true, dup));
    // With one, as it says.
    let j = JobState::new(cross, Some(HashSet::from([DISK])));
    assert_eq!(subdir_skip(&sh, &t, &j, &m, true), (true, dup));
}

#[test]
fn each_subdirectory_gets_an_id_and_a_task() {
    let dir = tempfile::tempdir().unwrap();
    fs::create_dir(dir.path().join("a")).unwrap();
    fs::create_dir(dir.path().join("b")).unwrap();
    fs::write(dir.path().join("a/f"), b"x").unwrap();
    let sh = Shared::new(Local::default(), Limits::default());
    sh.start_job(JobId(1), dir.path(), JobOptions::default());
    let mut w = Worker::new(&sh, true);
    let msgs = run(&sh, &mut w, &Inbox::default());
    let listed = msgs
        .iter()
        .find_map(|m| match m {
            ScanMsg::Chunk(c) if c.dir == DirId::ROOT && !c.subdirs.is_empty() => Some(c),
            _ => None,
        })
        .unwrap();
    assert_eq!(listed.first_subdir, DirId(1));
    let mut names: Vec<&[u8]> = listed.subdirs.iter().map(|s| listed.name(s.name)).collect();
    names.sort_unstable();
    assert_eq!(names, [b"a", b"b"]);
    assert!(
        listed
            .subdirs
            .iter()
            .all(|s| !s.mount_root && s.skip.is_none())
    );
    for d in [DirId::ROOT, DirId(1), DirId(2)] {
        let close = close_of(&msgs, d);
        assert!(
            matches!(
                close,
                ScanMsg::Close {
                    status: ListingStatus::Complete,
                    ..
                }
            ),
            "{close:?}"
        );
    }
    let files: usize = msgs
        .iter()
        .filter_map(|m| match m {
            ScanMsg::Chunk(c) => Some(c.files.len()),
            _ => None,
        })
        .sum();
    assert_eq!(files, 1);
    assert_eq!(sh.outstanding(), (0, 0));
}
