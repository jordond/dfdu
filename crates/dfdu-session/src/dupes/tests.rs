use super::*;

/// A result with `/r/a` (oldest), `/r/bb/b` and `/r/c` (newest) in one group, and `/r/x` and
/// `/r/y` in another; every file still in the tree.
fn found() -> (Found, Nodes) {
    let mut nodes = Nodes::new();
    let mut bits = 0;
    let mut file = |path: &str, mtime: i64| {
        bits += 1;
        nodes.insert(path.into(), Some(NodeRef::from_bits(bits)));
        Candidate {
            path: path.into(),
            size: 1 << 20,
            alloc: 1 << 20,
            dev_ino: (1, bits),
            mtime,
            links: 0,
            private: None,
        }
    };
    let groups: &[&[(&str, i64)]] = &[
        &[("/r/a", 10), ("/r/bb/b", 20), ("/r/c", 30)],
        &[("/r/x", 5), ("/r/y", 5)],
    ];
    let groups = (groups.iter())
        .map(|files| Group {
            size: 1 << 20,
            hash: [0; 32],
            files: files.iter().map(|&(p, mtime)| file(p, mtime)).collect(),
        })
        .collect();
    let found = Found {
        groups,
        compared: 5,
        ..Found::default()
    };
    (found, nodes)
}

fn paths(files: &[(Candidate, NodeRef)]) -> Vec<String> {
    files
        .iter()
        .map(|(c, _)| c.path.display().to_string())
        .collect()
}

#[test]
fn the_keep_rule_picks_the_copy_kept() {
    let (found, mut nodes) = found();
    let group = &found.groups[0];
    let kept_path = |keep, nodes: &Nodes| kept(group, keep, nodes).map(|i| &group.files[i].path);
    for (keep, path) in [
        (Keep::Oldest, "/r/a"),
        (Keep::Newest, "/r/c"),
        (Keep::ShortestPath, "/r/a"),
        (Keep::FirstPath, "/r/a"),
    ] {
        assert_eq!(kept_path(keep, &nodes), Some(&path.into()), "{keep:?}");
    }
    // The copy kept is one still in the tree.
    nodes.insert("/r/a".into(), None);
    assert_eq!(kept_path(Keep::Oldest, &nodes), Some(&"/r/bb/b".into()));
    assert_eq!(paths(&copies(group, Keep::Oldest, &nodes)), ["/r/c"]);
}

#[test]
fn the_rules_cycle_through_all() {
    let mut keep = Keep::default();
    for want in Keep::ALL {
        assert_eq!(keep, want);
        keep = keep.next();
    }
    assert_eq!(keep, Keep::default(), "round to the first");
}

#[test]
fn suggested_leaves_the_kept_copies_and_the_clones() {
    let (mut found, nodes) = found();
    assert_eq!(
        paths(&suggested(&found, Keep::Oldest, &nodes)),
        ["/r/bb/b", "/r/c", "/r/y"]
    );
    for c in &mut found.groups[1].files {
        c.private = Some(0);
    }
    assert_eq!(
        paths(&suggested(&found, Keep::Newest, &nodes)),
        ["/r/a", "/r/bb/b"]
    );
}
