use super::*;

fn far() -> Instant {
    Instant::now() + Duration::from_secs(60)
}

#[test]
fn a_listing_puts_directories_first() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::create_dir(tmp.path().join("sub")).unwrap();
    std::fs::write(tmp.path().join("file"), b"").unwrap();
    let got = read_dir(tmp.path(), MAX_ENTRIES, MAX_NAME, far()).unwrap();
    assert_eq!(
        got,
        [
            ListEntry {
                name: b"sub".to_vec(),
                dir: true
            },
            ListEntry {
                name: b"file".to_vec(),
                dir: false
            },
        ]
    );
    assert!(read_dir(&tmp.path().join("gone"), MAX_ENTRIES, MAX_NAME, far()).is_err());
}

#[test]
fn a_path_that_cannot_be_listed_is_refused_before_reading() {
    assert_eq!(check_path(b""), Err("no folder to list"));
    assert_eq!(
        check_path(&vec![b'/'; MAX_PATH + 1]),
        Err("the path is too long")
    );
    assert_eq!(check_path(b"relative"), Err("the path is not absolute"));
    let tmp = tempfile::tempdir().unwrap();
    let abs = dfdu_tree::name_bytes(tmp.path().as_os_str()).into_owned();
    assert_eq!(check_path(&abs), Ok(()));
}

#[test]
fn a_read_that_does_not_return_is_left_behind() {
    let got = within(Duration::from_millis(20), || {
        thread::sleep(Duration::from_secs(2));
    });
    assert_eq!(got, Err(Late::Timeout));
    assert_eq!(within(Duration::from_secs(5), || 7), Ok(7));
    assert_eq!(Late::NoThread.text("late"), NO_THREAD);
    assert_eq!(Late::Timeout.text("late"), "late");
}

fn names(got: &[ListEntry]) -> Vec<(&[u8], bool)> {
    got.iter().map(|e| (e.name.as_slice(), e.dir)).collect()
}

#[test]
fn a_listing_stops_at_the_entry_cap_keeping_directories_first() {
    let tmp = tempfile::tempdir().unwrap();
    for d in ["d1", "d2"] {
        std::fs::create_dir(tmp.path().join(d)).unwrap();
    }
    for f in ["f1", "f2", "f3"] {
        std::fs::write(tmp.path().join(f), b"").unwrap();
    }
    let got = read_dir(tmp.path(), 3, MAX_NAME, far()).unwrap();
    assert_eq!(got.len(), 3);
    assert!(got[0].dir && got[1].dir && !got[2].dir, "{got:?}");
    // With as many directories as the cap, no file is left.
    let got = read_dir(tmp.path(), 2, MAX_NAME, far()).unwrap();
    assert!(got.iter().all(|e| e.dir), "{got:?}");
    assert_eq!(got.len(), 2);
}

#[test]
fn a_listing_leaves_out_names_past_the_longest() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::create_dir(tmp.path().join("short")).unwrap();
    std::fs::create_dir(tmp.path().join("longer_name")).unwrap();
    let got = read_dir(tmp.path(), MAX_ENTRIES, 5, far()).unwrap();
    assert_eq!(names(&got), [(&b"short"[..], true)]);
}

#[test]
fn a_listing_past_its_deadline_stops() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::create_dir(tmp.path().join("sub")).unwrap();
    let got = read_dir(tmp.path(), MAX_ENTRIES, MAX_NAME, Instant::now()).unwrap();
    assert!(got.is_empty(), "{got:?}");
}

#[cfg(unix)]
#[test]
fn a_link_to_a_directory_is_listed_as_one() {
    let tmp = tempfile::tempdir().unwrap();
    let p = tmp.path();
    std::fs::create_dir(p.join("real")).unwrap();
    std::fs::write(p.join("file"), b"").unwrap();
    std::os::unix::fs::symlink(p.join("real"), p.join("to_dir")).unwrap();
    std::os::unix::fs::symlink(p.join("file"), p.join("to_file")).unwrap();
    std::os::unix::fs::symlink(p.join("gone"), p.join("dangling")).unwrap();
    let mut got = read_dir(p, MAX_ENTRIES, MAX_NAME, far()).unwrap();
    got.sort_by(|a, b| a.name.cmp(&b.name));
    assert_eq!(
        names(&got),
        [
            (&b"dangling"[..], false),
            (&b"file"[..], false),
            (&b"real"[..], true),
            (&b"to_dir"[..], true),
            (&b"to_file"[..], false),
        ]
    );
}

fn disk(point: &str, fstype: &str) -> dfdu_sys::Disk {
    dfdu_sys::Disk {
        mount_point: PathBuf::from(point),
        also_at: Vec::new(),
        fstype: fstype.into(),
        source: String::new(),
        label: None,
        class: dfdu_sys::FsClass::Local,
        space: None,
        role: None,
        shares_space_with: Vec::new(),
    }
}

#[test]
fn mounts_the_wire_cannot_carry_are_left_out() {
    let mut disks = vec![
        disk("/", "ext4"),
        disk("", "ext4"),
        disk(&"/x".repeat(MAX_PATH), "ext4"),
        disk("/mnt", &"t".repeat(MAX_FSTYPE + 1)),
    ];
    disks[0].space = Some(dfdu_sys::Space {
        total: 10,
        used: 4,
        available: 6,
        purgeable: None,
    });
    assert_eq!(
        host_mounts(&disks),
        [HostMount {
            point: b"/".to_vec(),
            fstype: "ext4".into(),
            used: Some(4),
            total: Some(10),
        }]
    );
    let many: Vec<_> = (0..=MAX_MOUNTS)
        .map(|i| disk(&format!("/m{i}"), "nfs"))
        .collect();
    assert_eq!(host_mounts(&many).len(), MAX_MOUNTS);
}
