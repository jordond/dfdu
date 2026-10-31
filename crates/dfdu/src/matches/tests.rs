use dfdu_rules::{Action, Env, Platform};
use dfdu_tree::RuleIx;

use super::*;

pub(crate) const DAY: i64 = 86_400;

/// The built-in rules of Linux, the last of them made to report its matches only (none of the
/// built-in rules does).
pub(crate) fn rules() -> RuleSet {
    let env = Env::with(Platform::Linux, &[("HOME", "/home/u")]);
    let mut rules = dfdu_rules::builtin(&env).0.rules().to_vec();
    rules.last_mut().expect("a rule").action = Action::Report;
    RuleSet::new(rules, &env)
}

/// The first rule whose action is `action` (by its name in rule files).
pub(crate) fn rule_with(rules: &RuleSet, action: &str) -> RuleIx {
    let r = rules.rules().iter().find(|r| r.action.as_str() == action);
    rules
        .index(&r.unwrap_or_else(|| panic!("a {action} rule")).id)
        .expect("its index")
}

/// A directory matched by `rule` at `safety`, of `disk` bytes on disk.
pub(crate) fn item(rule: RuleIx, safety: Safety, disk: u64) -> Item {
    Item {
        node: NodeRef::from_bits(1),
        path: HostPath::from("/home/u/x"),
        kind: Kind::Dir,
        class: Class {
            rule,
            safety,
            degraded: None,
            flags: 0,
        },
        disk,
        apparent: disk / 2,
        entries: 3,
        shared_out: 0,
        mtime: 0,
        scanning: false,
        recommended: false,
        matches_inside: 0,
        rest: vec![],
    }
}

#[test]
fn ages_are_whole_days_and_never_negative() {
    assert_eq!(age_days(10 * DAY, 10 * DAY), 0);
    assert_eq!(age_days(10 * DAY, 7 * DAY + 1), 2);
    assert_eq!(age_days(DAY, 5 * DAY), 0, "a match from the future is new");
}

/// The user caches with pip's cache (300 bytes) and other things (1000 bytes) in them: pip
/// is listed on its own, the user caches for the rest, and the total counts each once.
#[test]
fn caches_inside_the_user_caches_are_listed_on_their_own() {
    use dfdu_tree::Meta;
    let env = dfdu_rules::Env::with(dfdu_rules::Platform::MacOs, &[("HOME", "/Users/u")]);
    let rules = dfdu_rules::builtin(&env).0;
    let mut m = Model::default();
    let t = &mut m.tree;
    let meta = |kind, size, ino| Meta {
        kind,
        apparent: size,
        alloc: size,
        mtime: 0,
        dev: 1,
        ino,
        nlink: 1,
        mount: 0,
    };
    let root = t.new_root(b"/r", &meta(Kind::Dir, 0, 1), true);
    let mut ino = 1;
    let mut dir = |t: &mut dfdu_tree::Tree, parent, name: &str, file: u64| {
        ino += 2;
        let d = t.add_dir(parent, name.as_bytes(), &meta(Kind::Dir, 0, ino));
        let own = t.own(d);
        t.propagate_add(d, &own);
        let f = t.add_file(d, b"f", &meta(Kind::File, file, ino + 1), false, None);
        let own = t.own(f);
        t.propagate_add(d, &own);
        d
    };
    let cache = dir(t, root, "Caches", 1000);
    let pip = dir(t, cache, "pip", 300);
    for (ix, id) in [(cache, "sys.user-caches"), (pip, "pip.cache")] {
        let class = Class {
            rule: rules.index(id).expect(id),
            safety: Safety::Safe,
            degraded: None,
            flags: 0,
        };
        t.set_class(ix, Some(class));
    }
    let pip_ref = t.node_ref(pip);
    let all = Filter {
        min_size: None,
        category: vec![],
        older_than: None,
    };
    let list = items(&m, &rules, &all, 0);
    let paths: Vec<_> = list.iter().map(|i| i.path.clone()).collect();
    let caches_path = HostPath::from("/r").join(b"Caches");
    assert_eq!(paths, [caches_path.clone(), caches_path.join(b"pip")]);
    let (caches, pip_item) = (&list[0], &list[1]);
    assert_eq!((caches.disk, caches.matches_inside), (1000, 1));
    assert_eq!(pip_item.disk, 300);
    assert_eq!(reclaimable(&list), m.tree.total(cache).alloc);
    // Cleaning the user caches up deletes their rest, not pip's cache.
    let rest = caches.targets();
    assert_eq!(rest.len(), 1);
    assert!(!rest.contains(&pip_ref) && !rest.contains(&caches.node));
    assert_eq!(pip_item.targets(), [pip_ref]);
    // Left out by a filter, pip's cache still is not counted with the user caches.
    let sys = Filter {
        category: vec!["sys".into()],
        ..all
    };
    let list = items(&m, &rules, &sys, 0);
    assert_eq!(list.len(), 1);
    assert_eq!(reclaimable(&list), 1000);
}

/// A match whose rule has a guard counts as checked once its class says so; one whose rule has
/// none counts at once.
#[test]
fn guarded_matches_wait_for_their_checks() {
    use dfdu_tree::Meta;
    let env = dfdu_rules::Env::with(dfdu_rules::Platform::Linux, &[("HOME", "/home/u")]);
    let rules = Arc::new(dfdu_rules::builtin(&env).0);
    let guarded = |want: bool| {
        let i = rules.rules().iter().position(|r| {
            let g = &r.guard;
            (g.not_vcs_tracked || !g.not_running.is_empty()) == want
        });
        rules
            .index(&rules.rules()[i.expect("a rule")].id)
            .expect("its index")
    };
    let mut m = Model {
        rules: Some(rules.clone()),
        ..Model::default()
    };
    let meta = |ino| Meta {
        kind: Kind::Dir,
        apparent: 0,
        alloc: 0,
        mtime: 0,
        dev: 1,
        ino,
        nlink: 1,
        mount: 0,
    };
    let root = m.tree.new_root(b"/r", &meta(1), true);
    let a = m.tree.add_dir(root, b"a", &meta(2));
    let class = |rule, flags| Class {
        rule,
        safety: Safety::Safe,
        degraded: None,
        flags,
    };
    m.tree.set_class(a, Some(class(guarded(false), 0)));
    assert!(guards_checked(&m));
    m.tree.set_class(a, Some(class(guarded(true), 0)));
    assert!(!guards_checked(&m));
    m.tree
        .set_class(a, Some(class(guarded(true), Class::CHECKED)));
    assert!(guards_checked(&m));
    let msg = checks_cut_short(CHECK_WAIT);
    assert!(msg.contains("did not finish in 60 s"), "{msg}");
}

/// An item's JSON names its rule and action, its sizes and flags; the native command and the
/// matches inside it only when there are.
#[test]
fn an_item_in_json_names_its_rule_its_sizes_and_its_flags() {
    let rules = rules();
    let rule = rule_with(&rules, "delete");
    let mut it = item(rule, Safety::Regenerable, 4096);
    it.class.flags = Class::CHECKED | Class::IN_USE;
    it.mtime = 3 * DAY;
    let j = item_json(&rules, &it, 10 * DAY);
    let r = rules.rule(rule);
    assert_eq!(j["path"], "/home/u/x");
    assert_eq!(
        (j["type"].as_str(), j["rule"].as_str()),
        (Some("dir"), Some(r.id.as_str()))
    );
    assert_eq!(
        (j["safety"].as_str(), j["action"].as_str()),
        (Some("regenerable"), Some("delete"))
    );
    assert_eq!(
        (j["disk"].as_u64(), j["apparent"].as_u64()),
        (Some(4096), Some(2048))
    );
    assert_eq!(
        (j["entries"].as_u64(), j["age_days"].as_i64()),
        (Some(3), Some(7))
    );
    let flags = [
        "checked",
        "in_use",
        "vcs_tracked",
        "recommended",
        "scanning",
    ];
    let flags = flags.map(|f| j[f].as_bool());
    assert_eq!(flags, [true, true, false, false, false].map(Some));
    assert!(j.get("native_command").is_none() && j.get("matches_inside").is_none());
    assert!(j.get("degraded").is_none());

    let native = rule_with(&rules, "native");
    let mut it = item(native, Safety::Safe, 1);
    it.kind = Kind::File;
    it.matches_inside = 2;
    let j = item_json(&rules, &it, 0);
    assert_eq!(j["type"], "file");
    assert!(
        j["native_command"]
            .as_array()
            .is_some_and(|a| !a.is_empty()),
        "{j}"
    );
    assert_eq!(j["matches_inside"], 2);
}
