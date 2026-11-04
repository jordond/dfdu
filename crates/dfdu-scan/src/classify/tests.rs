use super::*;
use dfdu_rules::{Env, Firmlinks, Platform, builtin};
use dfdu_tree::{DirId, Meta};
use std::path::Path;

const DATA: &str = "/System/Volumes/Data";

fn rules() -> Arc<RuleSet> {
    let env = Env::with(Platform::MacOs, &[("HOME", "/Users/u")])
        .with_firmlinks(Firmlinks::parse("/Users\tUsers\n/usr/local\tusr/local\n"));
    Arc::new(builtin(&env).0)
}

/// What a scan rooted at `path` starts its root's listing with.
fn root(rules: &Arc<RuleSet>, path: &str) -> TaskClass {
    TaskClass::root(&ClassOpts {
        rules: rules.clone(),
        path: path.as_bytes().to_vec(),
        inside: Vec::new(),
        protection: rules.protected().state_of(path.as_bytes()),
        chain: Vec::new(),
    })
}

/// Classifies a whole listing of the directories `names` in the directory of `task`.
#[expect(clippy::cast_possible_truncation, reason = "short test names")]
fn list(rules: &Arc<RuleSet>, task: TaskClass, names: &[&str]) -> BlockClass {
    let meta = Meta {
        kind: Kind::Dir,
        apparent: 0,
        alloc: 0,
        mtime: 0,
        dev: 1,
        ino: 1,
        nlink: 1,
        mount: 0,
    };
    let mut buf = Vec::new();
    let mut subdirs = Vec::new();
    for n in names {
        subdirs.push(SubdirRec {
            name: NameSpan {
                off: buf.len() as u32,
                len: n.len() as u16,
            },
            meta,
            mount_root: false,
            skip: None,
        });
        buf.extend_from_slice(n.as_bytes());
    }
    let listing = Listing::new(rules.clone(), DirId(0), task);
    let parts = BlockParts {
        names: &buf,
        files: &[],
        subdirs: &subdirs,
        others: &[],
        first_subdir: DirId(1),
        whole: true,
    };
    listing.block(&parts, &|_, _| None, &|_| false)
}

fn mark(out: &BlockClass, i: u32) -> Option<Mark> {
    (out.marks.iter())
        .find(|(e, _)| *e == Entry::Subdir(i))
        .map(|&(_, m)| m)
}

fn task(out: &mut BlockClass, i: usize) -> TaskClass {
    *out.subdirs[i].take().expect("a task for the subdirectory")
}

/// A firmlinked directory of the Data volume is lifted out of `/System`'s protection and read
/// where the system volume shows it; the rest of the Data volume stays protected.
#[test]
fn firmlinked_data_directories_are_read_where_they_are_shown() {
    let rules = rules();
    let data = root(&rules, DATA);
    assert!(data.protection.protected(), "the Data volume is in /System");
    let mut out = list(&rules, data, &["Users", ".Spotlight-V100", "usr"]);
    assert_eq!(mark(&out, 0), Some(Mark::Excepted));
    assert_eq!(mark(&out, 1), None);
    assert_eq!(mark(&out, 2), None);
    let users = task(&mut out, 0);
    assert_eq!(users.path, b"/Users");
    assert_eq!(&users.path[users.name_at..], b"Users");
    assert!(!users.protection.protected());
    assert!(task(&mut out, 1).protection.protected());
    let usr = task(&mut out, 2);
    assert_eq!(usr.path, format!("{DATA}/usr").as_bytes());
    assert!(usr.protection.protected());

    // `/usr/local` is excepted from `/usr` through its firmlink as on the system volume.
    let mut out = list(&rules, usr, &["local", "share"]);
    assert_eq!(mark(&out, 0), Some(Mark::Excepted));
    assert_eq!(mark(&out, 1), None);
    let local = task(&mut out, 0);
    assert_eq!(local.path, b"/usr/local");
    assert!(!local.protection.protected());
    assert!(task(&mut out, 1).protection.protected());
}

/// Below a firmlink the home directory's protected paths still protect, and what the rules
/// recognise is classified, exactly as on the system volume's paths.
#[test]
fn below_a_firmlink_rules_and_protection_match_the_shown_paths() {
    let rules = rules();
    let walk = |top: TaskClass| {
        let mut out = list(&rules, top, &["u"]);
        let home = task(&mut out, 0);
        let mut out = list(&rules, home, &["Library", ".ssh"]);
        let ssh = mark(&out, 1);
        let library = task(&mut out, 0);
        let mut out = list(&rules, library, &["Caches", "Keychains"]);
        let keychains = mark(&out, 1);
        let out = list(&rules, task(&mut out, 0), &["pip"]);
        (ssh, mark(&out, 0), keychains)
    };
    let mut out = list(&rules, root(&rules, DATA), &["Users"]);
    let through = walk(task(&mut out, 0));
    let shown = walk(root(&rules, "/Users"));
    assert_eq!(through, shown);
    let (ssh, pip, keychains) = through;
    assert_eq!(ssh, Some(Mark::Protected));
    assert_eq!(keychains, Some(Mark::Protected));
    assert!(matches!(pip, Some(Mark::Class(_))), "{pip:?}");
}

/// A scan rooted at a firmlinked directory of the Data volume is judged where it is shown,
/// not as a directory of `/System`.
#[test]
fn a_root_at_a_firmlink_is_judged_where_it_is_shown() {
    let rules = rules();
    let opts = ClassOpts::for_root(rules.clone(), Path::new(&format!("{DATA}/Users")));
    assert_eq!(opts.path, b"/Users");
    assert!(!opts.protection.protected());
    let opts = ClassOpts::for_root(rules, Path::new(&format!("{DATA}/.Spotlight-V100")));
    assert!(opts.protection.protected());
}
