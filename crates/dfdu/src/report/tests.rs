use super::*;

#[test]
fn sizes_and_ages_parse() {
    assert_eq!(size_arg("100M"), Ok(100 << 20));
    assert_eq!(size_arg("1.5GiB"), Ok(3 << 29));
    assert_eq!(size_arg("500 KB"), Ok(500_000));
    assert_eq!(size_arg("0"), Ok(0));
    assert!(size_arg("lots").is_err());
    assert_eq!(age_arg("30d"), Ok(30 * 86_400));
    assert_eq!(age_arg("2w"), Ok(14 * 86_400));
    assert_eq!(age_arg("12h"), Ok(12 * 3_600));
    assert_eq!(age_arg("3"), Ok(3 * 86_400));
    assert!(age_arg("3y").is_err() && age_arg("d").is_err());
    assert!(category_arg("dev").is_ok() && category_arg("dev.build").is_ok());
    assert!(category_arg("de").is_err() && category_arg("build-cache").is_err());
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
            flags: dfdu_tree::ClassFlags::NONE,
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
    m.tree
        .set_class(a, Some(class(guarded(false), ClassFlags::NONE)));
    assert!(guards_checked(&m));
    m.tree
        .set_class(a, Some(class(guarded(true), ClassFlags::NONE)));
    assert!(!guards_checked(&m));
    m.tree
        .set_class(a, Some(class(guarded(true), ClassFlags::CHECKED)));
    assert!(guards_checked(&m));
    let msg = checks_cut_short(CHECK_WAIT);
    assert!(msg.contains("did not finish in 60 s"), "{msg}");
}
