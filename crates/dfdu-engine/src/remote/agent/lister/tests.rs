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
