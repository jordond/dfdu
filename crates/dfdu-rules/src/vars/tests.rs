use super::*;

/// What [`Env::expand`] returns for a pattern that can match nothing here.
const NOTHING: [String; 0] = [];

#[test]
fn used_by_names_the_variables_rules_can_read() {
    let e = Env::with(
        Platform::Linux,
        &[
            ("HOME", "/h"),
            ("GOPATH", "/g"),
            ("SECRET", "x"),
            ("A", "/a"),
        ],
    );
    let rules = "paths = [\"{env:GOPATH}/pkg\", \"{env:NONE|{env:A}}/b\"]";
    assert_eq!(
        e.used_by(&[rules]),
        [("A", "/a"), ("GOPATH", "/g"), ("HOME", "/h")].map(|(k, v)| (k.to_owned(), v.to_owned()))
    );
}

fn mac() -> Env {
    Env::with(
        Platform::MacOs,
        &[("HOME", "/Users/u"), ("TMPDIR", "/var/folders/x/T/")],
    )
}

#[test]
fn named_variables() {
    let e = mac();
    assert_eq!(
        e.expand("{cache}/pip").unwrap(),
        ["/Users/u/Library/Caches/pip"]
    );
    assert_eq!(e.expand("{temp}").unwrap(), ["/var/folders/x/T"]);
    let linux = Env::with(
        Platform::Linux,
        &[("HOME", "/home/u"), ("XDG_CACHE_HOME", "/c")],
    );
    assert_eq!(linux.expand("{cache}/x").unwrap(), ["/c/x"]);
    assert_eq!(linux.expand("{config}").unwrap(), ["/home/u/.config"]);
    assert_eq!(
        linux.expand("{program_files}/**").unwrap(),
        ["/opt/**", "/usr/**"]
    );
    let win = Env::with(
        Platform::Windows,
        &[
            ("USERPROFILE", r"C:\Users\u"),
            ("LOCALAPPDATA", r"C:\Users\u\AppData\Local"),
        ],
    );
    assert_eq!(
        win.expand("{cache}/Temp").unwrap(),
        ["C:/Users/u/AppData/Local/Temp"]
    );
    // No ProgramFiles in this environment: the pattern matches nothing.
    assert_eq!(win.expand("{program_files}/x").unwrap(), NOTHING);
}

#[test]
fn env_with_nested_fallback() {
    let e = mac();
    let p = "{env:ANDROID_AVD_HOME|{env:ANDROID_USER_HOME|{home}/.android}/avd}/*.avd";
    assert_eq!(e.expand(p).unwrap(), ["/Users/u/.android/avd/*.avd"]);
    let e = Env::with(
        Platform::Linux,
        &[("HOME", "/h"), ("ANDROID_USER_HOME", "/a")],
    );
    assert_eq!(e.expand(p).unwrap(), ["/a/avd/*.avd"]);
    assert_eq!(e.expand("{env:NOPE}/x").unwrap(), NOTHING);
}

#[test]
fn match_variables_stay_and_errors_are_reported() {
    assert_eq!(
        mac().expand("{name_stem}.ini").unwrap(),
        ["{name_stem}.ini"]
    );
    let unknown = mac().expand("{nope}").unwrap_err();
    assert_eq!(unknown, ExpandError::Unknown("nope".into()));
    assert_eq!(unknown.to_string(), "unknown variable `{nope}`");
    let unclosed = mac().expand("{home").unwrap_err();
    assert_eq!(unclosed.to_string(), "unclosed `{` in `{home`");
    // No home directory: nothing below it can match.
    assert_eq!(
        Env::with(Platform::Linux, &[]).expand("{home}/x").unwrap(),
        NOTHING
    );
}

#[cfg(unix)]
#[test]
fn link_targets_follow_links_in_the_fixed_part() {
    let tmp = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(tmp.path()).unwrap();
    let r = root.to_str().unwrap();
    std::fs::create_dir_all(root.join("data/avd")).unwrap();
    std::fs::create_dir_all(root.join("home")).unwrap();
    std::os::unix::fs::symlink(root.join("data"), root.join("home/.android")).unwrap();
    let e = Env::with(Platform::Linux, &[]).resolving_links();
    // Through the link, also when what follows it is not there (yet).
    let p = format!("{r}/home/.android/avd/*.avd");
    assert_eq!(
        e.link_targets(p.clone()),
        [p, format!("{r}/data/avd/*.avd")]
    );
    let p = format!("{r}/home/.android/nope/x");
    assert_eq!(e.link_targets(p.clone()), [p, format!("{r}/data/nope/x")]);
    // No link, a pattern that is not anchored, or links not followed: as it is.
    let p = format!("{r}/home/x/**");
    assert_eq!(e.link_targets(p.clone()), [p]);
    assert_eq!(e.link_targets("**/lib".into()), ["**/lib"]);
    let p = format!("{r}/home/.android/avd");
    assert_eq!(Env::with(Platform::Linux, &[]).link_targets(p.clone()), [p]);
}

/// What one machine followed, another expands alike without looking at its own disks.
#[cfg(unix)]
#[test]
fn links_followed_expand_alike_elsewhere() {
    let tmp = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(tmp.path()).unwrap();
    let r = root.to_str().unwrap();
    std::fs::create_dir_all(root.join("data")).unwrap();
    std::os::unix::fs::symlink(root.join("data"), root.join("tool")).unwrap();
    let here = Env::with(Platform::Linux, &[]).resolving_links();
    let p = format!("{r}/tool/cache");
    let both = here.link_targets(p.clone());
    assert_eq!(both, [p.clone(), format!("{r}/data/cache")]);
    assert_eq!(
        here.link_targets(format!("{r}/plain")),
        [format!("{r}/plain")]
    );
    let followed = here.links_followed();
    assert_eq!(followed, [(p.clone(), format!("{r}/data/cache"))]);
    // The link is gone; the record still leads where it did.
    std::fs::remove_file(root.join("tool")).unwrap();
    let there = Env::with(Platform::Linux, &[]).with_links(&followed);
    assert_eq!(there.link_targets(p.clone()), both);
    assert_eq!(
        there.link_targets(format!("{r}/plain")),
        [format!("{r}/plain")]
    );
    assert_eq!(there.links_followed(), followed);
    assert_eq!(Env::with(Platform::Linux, &[]).links_followed(), []);
}
