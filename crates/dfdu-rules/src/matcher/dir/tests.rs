use super::super::tests::mac;
use super::*;

/// `may_match_below` for pattern `pat` and the directory `dir`.
fn below(pat: &str, dir: &str, fold: bool) -> (bool, bool) {
    let g = Glob::new(pat, fold).unwrap();
    let dir = with_slash(dir.as_bytes());
    may_match_below(&g, &dir, depth_below(&dir), fold)
}

#[test]
fn entries_count_the_components_of_their_directory_plus_one() {
    assert_eq!(depth_below(&with_slash(b"/")), 2);
    assert_eq!(depth_below(&with_slash(b"/Users/u")), 4);
    assert_eq!(with_slash(b"/a/"), b"/a/");
}

#[test]
fn a_pattern_reaches_the_directories_on_its_path_and_its_depth() {
    let lib = "/Users/*/Library";
    // An entry of /Users/u can be Library. The second answer counts the entries themselves.
    assert_eq!(below(lib, "/Users/u", false), (true, true));
    // Above it: not here, but below.
    assert_eq!(below(lib, "/", false), (false, true));
    assert_eq!(below(lib, "/Users", false), (false, true));
    // Past its depth, or off its path: never.
    assert_eq!(below(lib, "/Users/u/Library", false), (false, false));
    assert_eq!(below(lib, "/opt", false), (false, false));
    // `**` matches at any depth below its prefix.
    assert_eq!(below("**/node_modules", "/a/b/c", false), (true, true));
    assert_eq!(below("/srv/**/cache", "/srv/x/y", false), (true, true));
    assert_eq!(below("/srv/**/cache", "/opt", false), (false, false));
}

#[test]
fn folded_case_compares_the_path_without_case() {
    let lib = "/Users/*/Library";
    assert_eq!(below(lib, "/USERS/u", true), (true, true));
    assert_eq!(below(lib, "/USERS/u", false), (false, false));
}

/// Handing each listing's [`Reach`] down to its subdirectories finds the same matches and
/// protection as trying every pattern, along paths where rules and protected paths match.
#[test]
fn reach_handed_down_matches_trying_every_pattern() {
    let set = mac();
    let paths = [
        "/Users/u/Library/Caches/com.apple.Safari/fsCachedData",
        "/Users/u/Library/Developer/Xcode/DerivedData/App-abc/Build",
        "/Users/u/Library/Keychains/x",
        "/Users/u/.npm/_cacache/content-v2",
        "/Users/u/.cargo/registry/cache/index",
        "/Users/u/.android/avd/pixel.avd",
        "/System/Library/Caches/x",
        "/private/var/folders/x/T/y",
        "/usr/local/lib/node_modules/npm",
        "/Applications/App.app/Contents/Resources",
    ];
    let names = [
        "Caches",
        "Library",
        "DerivedData",
        "_cacache",
        "cache",
        "avd",
        "pixel.avd",
        "pixel.ini",
        "node_modules",
        "Keychains",
        "Contents",
        "x",
        "T",
        "y",
        "target",
        "Cargo.toml",
        "package.json",
        "Build",
        "fsCachedData",
        "com.apple.Safari",
    ];
    let sibling = |n: &[u8]| names.iter().any(|x| x.as_bytes() == n);
    let (mut matched, mut protected) = (0, 0);
    for path in paths {
        let parts: Vec<&str> = path.split('/').filter(|p| !p.is_empty()).collect();
        let mut reach: Option<Reach> = None;
        let mut prot = protected::State::Off;
        for depth in 0..=parts.len() {
            let parent = format!("/{}", parts[..depth].join("/"));
            let full = set.dir(parent.as_bytes());
            let pruned = set.dir_in(parent.as_bytes(), reach.as_ref());
            for name in names {
                for is_dir in [true, false] {
                    let c = Candidate {
                        parent: parent.as_bytes(),
                        name: name.as_bytes(),
                        is_dir,
                        sibling: &sibling,
                        inside: &[],
                        protection: prot,
                        partial: false,
                        stat: None,
                        link: None,
                    };
                    let v = full.classify(&c);
                    match &v {
                        Verdict::Hits(h)
                            if h.iter().any(|h| !set.rule(h.rule).paths.is_empty()) =>
                        {
                            matched += 1;
                        }
                        Verdict::Protected(_) => protected += 1,
                        _ => {}
                    }
                    assert_eq!(v, pruned.classify(&c), "{parent}/{name}");
                }
            }
            if let Some(next) = parts.get(depth) {
                prot = match full.classify(&Candidate {
                    parent: parent.as_bytes(),
                    name: next.as_bytes(),
                    is_dir: true,
                    sibling: &sibling,
                    inside: &[],
                    protection: prot,
                    partial: false,
                    stat: None,
                    link: None,
                }) {
                    Verdict::Protected(s) => s,
                    _ => protected::State::Off,
                };
            }
            reach = Some(pruned.reach().clone());
        }
    }
    assert!(
        matched >= 5 && protected >= 20,
        "{matched} path matches, {protected} protected"
    );
}
