//! Fixture tests for the built-in rules: each listing below is a directory as the scan sees it,
//! and each case says what an entry in it classifies as, including the degrade cases.

use super::*;
use crate::{Platform, builtin};

fn mac() -> RuleSet {
    let (set, errors) = builtin(&Env::with(
        Platform::MacOs,
        &[("HOME", "/Users/u"), ("TMPDIR", "/private/var/folders/x/T")],
    ));
    assert!(errors.is_empty(), "{errors:#?}");
    set
}

fn linux() -> RuleSet {
    let (set, errors) = builtin(&Env::with(Platform::Linux, &[("HOME", "/home/u")]));
    assert!(errors.is_empty(), "{errors:#?}");
    set
}

/// A directory listing: its path and the names in it (directories end in `/`).
struct Listing<'a> {
    path: &'a str,
    names: &'a [&'a str],
}

impl Listing<'_> {
    fn has(&self, name: &[u8]) -> bool {
        self.names
            .iter()
            .any(|n| n.trim_end_matches('/').as_bytes() == name)
    }
}

/// Like [`hits`], without the CACHEDIR.TAG rule, which every directory is a candidate for.
fn classify(set: &RuleSet, l: &Listing, name: &str, inside: &[&str]) -> Vec<String> {
    let mut v = hits(set, l, name, inside);
    v.retain(|h| !h.starts_with("generic.cachedir-tag"));
    v
}

/// The ids of the rules `name` in `l` matches, best first, with their safety; `protected` for
/// a protected path. `inside` is the rules matched by the directories above.
fn hits(set: &RuleSet, l: &Listing, name: &str, inside: &[&str]) -> Vec<String> {
    let entry = l
        .names
        .iter()
        .find(|n| n.trim_end_matches('/') == name)
        .unwrap_or_else(|| panic!("{name} is not in {}", l.path));
    let inside: Vec<u16> = inside
        .iter()
        .map(|id| set.rules().iter().position(|r| r.id == *id).expect(id) as u16)
        .collect();
    let sibling = |n: &[u8]| l.has(n);
    let c = Candidate {
        parent: l.path.as_bytes(),
        name: name.as_bytes(),
        is_dir: entry.ends_with('/'),
        sibling: &sibling,
        inside: &inside,
        protection: set.protected().state_of(l.path.as_bytes()),
        partial: false,
    };
    match set.classify(&c) {
        Verdict::None => Vec::new(),
        Verdict::Protected(_) => vec!["protected".into()],
        Verdict::Hits(hits) => hits
            .iter()
            .map(|h| format!("{} {}", set.rule(h.rule).id, h.safety.as_str()))
            .collect(),
    }
}

/// Whether rule `id` holds for an entry whose own listing has `children`; `tag` is the first
/// bytes of its CACHEDIR.TAG, if it has one.
fn inside_ok(set: &RuleSet, id: &str, name: &str, children: &[&str], tag: Option<&str>) -> bool {
    let i = set.rules().iter().position(|r| r.id == id).expect(id) as u16;
    let child = |n: &[u8]| children.iter().any(|c| c.as_bytes() == n);
    let marker = |file: &[u8], len: usize| {
        (file == b"CACHEDIR.TAG")
            .then_some(tag)
            .flatten()
            .map(|t| t.as_bytes()[..len.min(t.len())].to_vec())
    };
    set.check_inside(i, name.as_bytes(), &child, &marker)
}

const TAG: &str = "Signature: 8a477f597d28d172789f06886806bc55\n# cache";

#[test]
fn rust_target_needs_a_manifest_and_build_output() {
    let set = mac();
    let proj = Listing {
        path: "/Users/u/dev/app",
        names: &["Cargo.toml", "Cargo.lock", "src/", "target/"],
    };
    assert_eq!(
        hits(&set, &proj, "target", &[]),
        ["rust.target regenerable", "generic.cachedir-tag safe"]
    );
    assert!(inside_ok(
        &set,
        "rust.target",
        "target",
        &["debug", "CACHEDIR.TAG"],
        Some(TAG)
    ));
    // A `target/` with none of cargo's entries is something else (a deploy target, say).
    assert!(!inside_ok(
        &set,
        "rust.target",
        "target",
        &["site.yml"],
        None
    ));
    // Without Cargo.toml next to it, only the CACHEDIR.TAG rule can still apply.
    let other = Listing {
        path: "/Users/u/dev/web",
        names: &["package.json", "target/"],
    };
    assert_eq!(
        hits(&set, &other, "target", &[]),
        ["generic.cachedir-tag safe"]
    );
    assert!(!inside_ok(
        &set,
        "generic.cachedir-tag",
        "target",
        &[],
        None
    ));
    assert!(!inside_ok(
        &set,
        "generic.cachedir-tag",
        "target",
        &["CACHEDIR.TAG"],
        Some("Signature: wrong")
    ));
    assert!(inside_ok(
        &set,
        "generic.cachedir-tag",
        "target",
        &["CACHEDIR.TAG"],
        Some(TAG)
    ));
}

#[test]
fn node_modules_drops_to_review_without_a_lockfile() {
    let set = linux();
    let locked = Listing {
        path: "/home/u/dev/site",
        names: &["package.json", "pnpm-lock.yaml", "node_modules/"],
    };
    assert_eq!(
        classify(&set, &locked, "node_modules", &[]),
        ["node.node_modules regenerable"]
    );
    let unlocked = Listing {
        path: "/home/u/dev/site",
        names: &["package.json", "node_modules/"],
    };
    assert_eq!(
        classify(&set, &unlocked, "node_modules", &[]),
        ["node.node_modules review"]
    );
    let bare = Listing {
        path: "/home/u/dev/x",
        names: &["node_modules/"],
    };
    assert!(classify(&set, &bare, "node_modules", &[]).is_empty());
    // A global install and a nested node_modules are not offered.
    let global = Listing {
        path: "/home/u/.local/lib",
        names: &["package.json", "node_modules/"],
    };
    assert!(classify(&set, &global, "node_modules", &[]).is_empty());
    let nested = Listing {
        path: "/home/u/dev/site/node_modules/left-pad",
        names: &["package.json", "node_modules/"],
    };
    assert!(classify(&set, &nested, "node_modules", &["node.node_modules"]).is_empty());
}

/// In one block of a wide directory a missing sibling may be in another block: the hit stays
/// open, with the safety it has if the sibling is missing, until [`RuleSet::siblings`] sees
/// the whole listing.
#[test]
fn siblings_in_other_blocks_leave_the_hit_open() {
    let set = linux();
    let node = set
        .rules()
        .iter()
        .position(|r| r.id == "node.node_modules")
        .unwrap() as u16;
    let block = Listing {
        path: "/home/u/dev/site",
        names: &["node_modules/"],
    };
    let sibling = |n: &[u8]| block.has(n);
    let c = Candidate {
        parent: block.path.as_bytes(),
        name: b"node_modules",
        is_dir: true,
        sibling: &sibling,
        inside: &[],
        protection: Default::default(),
        partial: true,
    };
    let Verdict::Hits(hits) = set.classify(&c) else {
        panic!("no hit");
    };
    assert_eq!((hits[0].rule, hits[0].open), (node, true));
    let whole =
        |names: &'static [&'static str]| move |n: &[u8]| names.iter().any(|x| x.as_bytes() == n);
    assert_eq!(
        set.siblings(
            node,
            b"node_modules",
            &whole(&["package.json", "yarn.lock"])
        ),
        Some((Safety::Regenerable, None))
    );
    assert_eq!(
        set.siblings(node, b"node_modules", &whole(&["package.json"])),
        Some((Safety::Review, Some(0)))
    );
    assert_eq!(set.siblings(node, b"node_modules", &whole(&[])), None);
    // Found in the block: nothing is left open.
    let found = Listing {
        path: "/home/u/dev/site",
        names: &["node_modules/", "package.json", "bun.lock"],
    };
    let sibling = |n: &[u8]| found.has(n);
    let Verdict::Hits(hits) = set.classify(&Candidate {
        sibling: &sibling,
        ..c
    }) else {
        panic!("no hit");
    };
    assert_eq!((hits[0].safety, hits[0].open), (Safety::Regenerable, false));
}

#[test]
fn python_venv_needs_pyvenv_cfg() {
    let set = linux();
    let l = Listing {
        path: "/home/u/dev/py",
        names: &["pyproject.toml", "uv.lock", ".venv/"],
    };
    assert_eq!(
        classify(&set, &l, ".venv", &[]),
        ["python.venv regenerable"]
    );
    assert!(inside_ok(
        &set,
        "python.venv",
        ".venv",
        &["bin", "lib", "pyvenv.cfg"],
        None
    ));
    assert!(!inside_ok(&set, "python.venv", ".venv", &["bin"], None));
    let loose = Listing {
        path: "/home/u/dev/py",
        names: &["main.py", "venv/"],
    };
    assert_eq!(classify(&set, &loose, "venv", &[]), ["python.venv review"]);
}

#[test]
fn path_rules_on_macos() {
    let set = mac();
    for (dir, name, want) in [
        (
            "/Users/u/Library/Developer/Xcode/DerivedData",
            "App-abcdef/",
            "xcode.derived-data safe",
        ),
        (
            "/Users/u/Library/Developer/Xcode/iOS DeviceSupport",
            "17.2 (21C62)/",
            "xcode.device-support regenerable",
        ),
        ("/Users/u/Library/Caches", "pip/", "pip.cache safe"),
        (
            "/Users/u/Library/Caches",
            "Homebrew/",
            "homebrew.cache safe",
        ),
        ("/Users/u/.npm", "_cacache/", "npm.cache safe"),
        (
            "/Users/u/.cargo/registry",
            "cache/",
            "cargo.registry regenerable",
        ),
        (
            "/Users/u/.gradle",
            "caches/",
            "gradle.user-caches regenerable",
        ),
        (
            "/Users/u/Library/Android/sdk/system-images",
            "android-36/",
            "android.system-images regenerable",
        ),
        ("/Users/u/Library", "Caches/", "sys.user-caches safe"),
        ("/Users/u", ".Trash/", "sys.trash review"),
        (
            "/Users/u/Library/Containers/com.docker.docker/Data/vms/0/data",
            "Docker.raw",
            "docker.desktop-disk manual",
        ),
        (
            "/Users/u/Downloads",
            "Xcode_16.xip",
            "content.installers-downloads review",
        ),
        (
            "/Users/u/Downloads/old",
            "Setup.DMG",
            "content.installers-downloads review",
        ),
    ] {
        let l = Listing {
            path: dir,
            names: &[name],
        };
        let name = name.trim_end_matches('/');
        assert_eq!(classify(&set, &l, name, &[]), [want], "{dir}/{name}");
    }
    // Case does not matter on macOS.
    let l = Listing {
        path: "/users/u/library/caches",
        names: &["PIP/"],
    };
    assert_eq!(classify(&set, &l, "PIP", &[]), ["pip.cache safe"]);
    // Excluded, and a file where a directory is wanted.
    let l = Listing {
        path: "/Users/u/Library/Developer/Xcode/DerivedData",
        names: &["ModuleCache.noindex/", "notes.txt"],
    };
    assert!(classify(&set, &l, "ModuleCache.noindex", &[]).is_empty());
    assert!(classify(&set, &l, "notes.txt", &[]).is_empty());
}

#[test]
fn avd_needs_its_ini_next_to_it() {
    let set = mac();
    let l = Listing {
        path: "/Users/u/.android/avd",
        names: &["pixel.avd/", "pixel.ini", "old.avd/"],
    };
    assert_eq!(classify(&set, &l, "pixel.avd", &[]), ["android.avd review"]);
    assert!(classify(&set, &l, "old.avd", &[]).is_empty());
    assert!(inside_ok(
        &set,
        "android.avd",
        "pixel.avd",
        &["config.ini"],
        None
    ));
}

#[test]
fn stop_descent_and_protected_paths() {
    let set = mac();
    // Inside a matched cache nothing more is classified...
    let l = Listing {
        path: "/Users/u/Library/Caches/pip/http",
        names: &["node_modules/", "package.json"],
    };
    assert!(classify(&set, &l, "node_modules", &["sys.user-caches", "pip.cache"]).is_empty());
    // ...but below the user caches, which do not stop the descent, the specific rules apply.
    let l = Listing {
        path: "/Users/u/Library/Caches",
        names: &["Yarn/"],
    };
    assert_eq!(
        classify(&set, &l, "Yarn", &["sys.user-caches"]),
        ["yarn.cache safe"]
    );
    // Protected paths win over every rule.
    let l = Listing {
        path: "/Users/u/dev/app",
        names: &[".git/", "Cargo.toml", "target/"],
    };
    assert_eq!(classify(&set, &l, ".git", &[]), ["protected"]);
    let l = Listing {
        path: "/Users/other/Library",
        names: &["Caches/"],
    };
    assert_eq!(classify(&set, &l, "Caches", &[]), ["protected"]);
    let l = Listing {
        path: "/Applications/Thing.app/Contents/Resources",
        names: &["package.json", "node_modules/"],
    };
    assert_eq!(classify(&set, &l, "node_modules", &[]), ["protected"]);
}

#[test]
fn unverified_path_rules_are_off_on_linux_but_name_rules_are_on() {
    let set = linux();
    assert!(set.get("pip.cache").is_none());
    assert!(set.get("rust.target").is_some());
    assert!(set.get("generic.cachedir-tag").is_some());
    assert!(set.get("xcode.derived-data").is_none());
    let mac = mac();
    assert!(
        mac.get("orbstack.data").is_none(),
        "not verified anywhere yet"
    );
    assert!(mac.get("pip.cache").is_some());
}

#[test]
fn every_builtin_rule_is_in_the_verification_table() {
    let doc = include_str!("../../../../docs/rules.md");
    let ids: Vec<String> = crate::load::BUILTIN
        .lines()
        .filter_map(|l| l.strip_prefix("id = \""))
        .map(|l| l.trim_end_matches('"').to_owned())
        .collect();
    assert!(ids.len() > 30);
    for id in ids {
        assert!(
            doc.contains(&format!("| `{id}` |")),
            "{id} is missing from docs/rules.md"
        );
    }
}

/// Cost of classifying one entry with the built-in pack, the work the applier will add per
/// entry. `cargo test --release -p dfdu-rules classify_cost -- --ignored --nocapture`.
#[test]
#[ignore]
fn classify_cost() {
    let set = mac();
    let names: Vec<String> = (0..100_000)
        .map(|i| match i % 10 {
            0 => format!("lib{i}.rs"),
            1 => "node_modules".into(),
            2 => format!("dir{i}"),
            3 => "target".into(),
            _ => format!("file-{i}.txt"),
        })
        .collect();
    let siblings = |n: &[u8]| n == b"package.json";
    let start = std::time::Instant::now();
    let mut matched = 0;
    let parent = b"/Users/u/dev/some/project/src";
    // One listing of 100 entries at a time, as the applier sees them.
    let mut dir = set.dir(parent);
    for (i, n) in names.iter().enumerate() {
        if i % 100 == 0 {
            dir = set.dir(parent);
        }
        let c = Candidate {
            parent: b"/Users/u/dev/some/project/src",
            name: n.as_bytes(),
            is_dir: i % 10 < 4,
            sibling: &siblings,
            inside: &[],
            protection: Default::default(),
            partial: false,
        };
        if dir.classify(&c) != Verdict::None {
            matched += 1;
        }
    }
    let per = start.elapsed() / names.len() as u32;
    println!("classify: {per:?} per entry ({matched} candidates)");
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
                    };
                    let v = full.classify(&c);
                    match &v {
                        Verdict::Hits(h)
                            if h.iter().any(|h| !set.rule(h.rule).paths.is_empty()) =>
                        {
                            matched += 1
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
