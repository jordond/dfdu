use super::*;

fn mac() -> Protected {
    Protected::new(&Env::with(
        Platform::MacOs,
        &[
            ("HOME", "/Users/u"),
            ("TMPDIR", "/private/var/folders/ab/xyz/T"),
        ],
    ))
}

fn linux() -> Protected {
    Protected::new(&Env::with(Platform::Linux, &[("HOME", "/home/u")]))
}

#[test]
fn names_protect_anywhere() {
    let p = linux();
    assert!(p.covers(b"/home/u/dev/proj/.git"));
    assert!(p.covers(b"/home/u/dev/proj/.git/objects/pack"));
    assert!(p.covers(b"/home/u/.ssh/id_ed25519"));
    assert!(p.covers(b"/home/u/vault.kdbx"));
    assert!(p.covers(b"/home/u/dev/app/.env.local"));
    assert!(!p.covers(b"/home/u/dev/proj/target"));
    assert!(!p.covers(b"/home/u/.cache/pip"));
}

#[test]
fn system_paths_and_their_exceptions() {
    let p = mac();
    assert!(p.covers(b"/System/Library"));
    assert!(p.covers(b"/usr/bin/true"));
    assert!(!p.covers(b"/usr/local/Cellar/x"));
    assert!(p.covers(b"/dev/fd/3"));
    assert!(p.covers(b"/etc/hosts"));
    assert!(p.covers(b"/var/db/x"));
    assert!(p.covers(b"/private/var/folders/ab/xyz/C/com.apple.x"));
    assert!(!p.covers(b"/private/var/folders/ab/xyz/T/tmp.123"));
    assert!(p.covers(b"/Users/u/Library/Containers/com.x/Data/Documents"));
    assert!(!p.covers(b"/Users/u/Library/Containers/com.x/Data/Library/Caches/y"));
    assert!(p.covers(b"/Applications/X.app/Contents/Resources"));
    assert!(p.covers(b"/Users/u/Pictures/Photos Library.photoslibrary/originals"));
    // Other users' homes are protected, our own is not.
    assert!(p.covers(b"/Users/other/Library/Caches"));
    assert!(!p.covers(b"/Users/u/Library/Caches/pip"));
    assert!(!p.covers(b"/Users/Shared/x"));
    // An exception does not lift a protected name below it.
    assert!(p.covers(b"/Users/u/.ssh/config"));
    assert!(p.covers(b"/Users/u/.cargo/bin/cargo"));
    assert!(!p.covers(b"/Users/u/.cargo/registry/cache"));
}

#[test]
fn linux_and_windows() {
    let p = linux();
    assert!(p.covers(b"/var/lib/docker/overlay2"));
    assert!(p.covers(b"/lib64/ld.so"));
    assert!(!p.covers(b"/var/cache/apt"));
    assert!(p.covers(b"/home/other/.cache"));
    assert!(!p.covers(b"/home/u/.cache"));
    assert!(p.covers(b"/home/u/.mozilla/firefox/abc.default/cookies.sqlite"));
    assert!(!p.covers(b"/home/u/.mozilla/firefox/abc.default/cache2/entries"));
    // The root itself is not a protected path, so nothing is protected by it.
    assert!(!p.covers(b"/tmp/x"));

    let w = Protected::new(&Env::with(
        Platform::Windows,
        &[
            ("USERPROFILE", r"C:\Users\u"),
            ("LOCALAPPDATA", r"C:\Users\u\AppData\Local"),
            ("APPDATA", r"C:\Users\u\AppData\Roaming"),
        ],
    ));
    assert!(w.covers(b"C:/Windows/System32/x.dll"));
    assert!(!w.covers(b"C:/Windows/Temp/x.tmp"));
    assert!(w.covers(b"C:/pagefile.sys"));
    assert!(w.covers(b"C:/Users/u/OneDrive - Work/doc.docx"));
    assert!(!w.covers(b"C:/Users/u/AppData/Local/Temp/x"));
    assert!(w.covers(b"C:/Users/other/AppData"));
}

/// Walking down a path one directory at a time with [`Protected::dir`] and
/// [`Protected::step_in`] gives what [`Protected::covers`] gives for the whole path.
#[test]
fn stepping_per_directory_agrees_with_whole_paths() {
    let p = mac();
    for path in [
        "/usr/local/Cellar/x",
        "/usr/bin/true",
        "/Users/u/Library/Containers/com.x/Data/Library/Caches/y",
        "/Users/u/Library/Containers/com.x/Data/Documents/z",
        "/Users/other/Library/Caches",
        "/Users/u/.ssh/config",
        "/Users/u/dev/a/.git/objects",
        "/private/var/folders/ab/xyz/T/tmp.1",
        "/Applications/X.app/Contents",
        "/Users/u/Library/Containers/com.docker.docker/Data/vms/0/data/Docker.raw",
    ] {
        let mut state = State::Off;
        let mut dir = b"".to_vec();
        for part in path.split('/').skip(1) {
            let d = p.dir(&[&dir[..], b"/"].concat());
            let full = [&dir[..], b"/", part.as_bytes()].concat();
            state = p.step_in(&d, state, part.as_bytes(), &|| full.clone());
            dir = full;
        }
        assert_eq!(state.protected(), p.covers(path.as_bytes()), "{path}");
    }
}
