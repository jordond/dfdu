use super::*;

fn unix(s: &[u8]) -> HostPath {
    HostPath::new(Host::Unix, s.to_vec())
}

fn win(s: &str) -> HostPath {
    HostPath::new(Host::Windows, s.as_bytes().to_vec())
}

/// Paths join with their host's separator, whichever machine this is.
#[test]
fn paths_join_with_the_hosts_separator() {
    assert_eq!(unix(b"/home/u").join(b"a\\b").as_bytes(), b"/home/u/a\\b");
    assert_eq!(unix(b"/").join(b"etc").as_bytes(), b"/etc");
    assert_eq!(
        win(r"C:\Users\u").join(b"proj").as_bytes(),
        br"C:\Users\u\proj"
    );
    assert_eq!(win(r"C:\").join(b"x").as_bytes(), br"C:\x");
    assert_eq!(win("C:/Users").join(b"x").as_bytes(), b"C:/Users\\x");
    assert_eq!(HostPath::empty(Host::Unix).join(b"a").as_bytes(), b"a");
}

/// `\` is part of a Unix name, and bytes that are not UTF-8 stay as they are.
#[test]
fn unix_names_keep_backslashes_and_bytes() {
    let p = unix(b"/home/u/a\\b/caf\xe9");
    assert_eq!(
        p.components().collect::<Vec<_>>(),
        [&b"home"[..], b"u", b"a\\b", b"caf\xe9"]
    );
    assert_eq!(p.file_name(), Some(&b"caf\xe9"[..]));
    assert_eq!(p.parent().unwrap().as_bytes(), b"/home/u/a\\b");
    assert_eq!(p.display(), "/home/u/a\\b/caf\u{fffd}");
}

#[test]
fn parents_and_names() {
    assert_eq!(unix(b"/a").parent().unwrap().as_bytes(), b"/");
    assert_eq!(unix(b"/").parent(), None);
    assert_eq!(unix(b"/").file_name(), None);
    assert_eq!(unix(b"a/b").parent().unwrap().as_bytes(), b"a");
    assert_eq!(unix(b"a").parent(), None);
    assert_eq!(win(r"C:\a").parent().unwrap().as_bytes(), br"C:\");
    assert_eq!(win(r"C:\a\b").parent().unwrap().as_bytes(), br"C:\a");
    assert_eq!(win(r"C:\").parent(), None);
    assert_eq!(win(r"C:\").file_name(), None);
    assert_eq!(win(r"\\?\C:\").parent(), None);
    assert_eq!(win(r"\\?\C:\x").parent().unwrap().as_bytes(), br"\\?\C:\");
    assert_eq!(
        win(r"\\srv\share\x").parent().unwrap().as_bytes(),
        br"\\srv\share\"
    );
    assert_eq!(win(r"\\srv\share\").parent(), None);
    assert_eq!(win(r"\\?\UNC\srv\share\x").file_name(), Some(&b"x"[..]));
    assert_eq!(win(r"\\?\UNC\srv\share\").file_name(), None);
    assert_eq!(win("C:/a/b").file_name(), Some(&b"b"[..]));
}

/// A path below another gives the components between, by whole components.
#[test]
fn relative_components() {
    let root = unix(b"/home/u");
    assert_eq!(unix(b"/home/u/a/b").rel(&root).unwrap(), [&b"a"[..], b"b"]);
    assert_eq!(unix(b"/home/u").rel(&root).unwrap(), Vec::<&[u8]>::new());
    assert_eq!(unix(b"/home/user").rel(&root), None);
    assert!(!unix(b"/home/user").starts_with(&root));
    assert_eq!(unix(b"/x").rel(&unix(b"/")).unwrap(), [&b"x"[..]]);
    let r = win(r"C:\Users\u");
    assert_eq!(win(r"C:\Users\u\a\b").rel(&r).unwrap(), [&b"a"[..], b"b"]);
    assert_eq!(win(r"C:\Users\u/a").rel(&r).unwrap(), [&b"a"[..]]);
    assert_eq!(win(r"C:\Users\uu").rel(&r), None);
    assert_eq!(win(r"C:\x").rel(&win(r"C:\")).unwrap(), [&b"x"[..]]);
}

#[test]
fn absolute_paths() {
    assert!(unix(b"/a").is_absolute());
    assert!(!unix(b"a/b").is_absolute());
    assert!(!unix(br"C:\a").is_absolute());
    assert!(win(r"C:\a").is_absolute());
    assert!(win("C:/a").is_absolute());
    assert!(win(r"\\srv\share").is_absolute());
    assert!(!win(r"C:a").is_absolute());
    assert!(!win(r"a\b").is_absolute());
}

#[test]
fn hosts_guessed_from_roots() {
    assert_eq!(Host::guess(b"/home/u"), Host::Unix);
    assert_eq!(Host::guess(br"C:\Users"), Host::Windows);
    assert_eq!(Host::guess(br"\\srv\share"), Host::Windows);
    assert_eq!(Host::guess(b"relative"), Host::LOCAL);
    for h in [Host::Unix, Host::Windows] {
        assert_eq!(Host::from_u8(h.to_u8()), Some(h));
    }
    assert_eq!(Host::from_u8(2), None);
}

/// This machine's paths go to the OS as they came.
#[test]
fn local_paths_round_trip() {
    let p = std::env::temp_dir().join("dfdu").join("x");
    let h = HostPath::local(&p);
    assert_eq!(h.host(), Host::LOCAL);
    assert_eq!(h.to_local(), p);
    assert_eq!(h.file_name(), Some(&b"x"[..]));
    assert_eq!(h.parent().unwrap().to_local(), p.parent().unwrap());
}
