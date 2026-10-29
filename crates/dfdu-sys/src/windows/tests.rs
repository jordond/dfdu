use super::*;

#[test]
fn verbatim_paths() {
    let v = |p: &str| verbatim_path(Path::new(p)).unwrap();
    assert_eq!(v(r"C:\a\..\b"), Path::new(r"\\?\C:\b"));
    assert_eq!(v(r"\\server\share\x"), Path::new(r"\\?\UNC\server\share\x"));
    assert_eq!(v(r"\\?\C:\a\..\b."), Path::new(r"\\?\C:\a\..\b."));
    assert_eq!(nt_path(Path::new(r"C:\x")).unwrap(), wide(r"\??\C:\x"));
}

/// Enumeration, link counts and stat agree on a small tree.
#[test]
fn reads_a_directory() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    std::fs::create_dir(root.join("sub")).unwrap();
    std::fs::write(root.join("f"), vec![1u8; 5000]).unwrap();
    std::fs::hard_link(root.join("f"), root.join("sub").join("g")).unwrap();
    let dir = open_dir(root, false).unwrap();
    let vol = volume_of(dir.as_handle()).unwrap();
    let mut reader = DirReader::default();
    let mut names = Vec::new();
    while let Some(e) = reader.next(dir.as_handle()).unwrap() {
        let name: Vec<u16> = e.name_units().collect();
        names.push((String::from_utf16(&name).unwrap(), e.stat));
    }
    names.sort_by(|a, b| a.0.cmp(&b.0));
    assert_eq!(names.len(), 2, "{names:?}");
    let (f, sub) = (&names[0].1, &names[1].1);
    assert_eq!(f.size, 5000);
    let fs = stat_path(&root.join("f"), false).unwrap();
    assert_eq!((fs.volume, fs.nlink, fs.size), (vol.serial, 2, 5000));
    assert_eq!(fs.file_id, f.file_id);
    assert_eq!(fs.alloc, f.alloc);
    assert_eq!(sub.attributes & win_attr::DIRECTORY, win_attr::DIRECTORY);
    if vol.hard_links {
        let query = LinkQuery::default();
        let links = query.query(dir.as_handle(), &wide("f"), false).unwrap();
        assert_eq!((links.nlink, links.size, links.alloc), (2, 5000, fs.alloc));
        let e = query
            .query(dir.as_handle(), &wide("missing"), false)
            .unwrap_err();
        assert_eq!(e.kind(), io::ErrorKind::NotFound);
    }
    let e = open_dir(&root.join("missing"), false).unwrap_err();
    assert_eq!(e.kind(), io::ErrorKind::NotFound);
}
