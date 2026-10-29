use std::path::Path;

use super::super::test_util::{add_dir, add_file, dir_meta};
use super::super::{SEP, Tree};
use crate::Host;

#[test]
fn cmp_paths_orders_like_path() {
    let mut t = Tree::new();
    let mut nodes = Vec::new();
    for root in ["/", "/r", "/r/", "/r-x"] {
        let r = t.new_root(root.as_bytes(), &dir_meta(), true);
        nodes.push(r);
        for a in ["a", "a-b", "a.b", "b", "ab"] {
            let d = add_dir(&mut t, r, a);
            nodes.push(d);
            for f in ["a", "a-b", "", "z"] {
                nodes.push(add_file(&mut t, d, f, 1));
            }
            let e = add_dir(&mut t, d, "");
            nodes.push(add_file(&mut t, e, "a", 1));
        }
    }
    let (mut ca, mut cb) = (Vec::new(), Vec::new());
    for &a in &nodes {
        t.ancestors(a, &mut ca);
        for &b in &nodes {
            t.ancestors(b, &mut cb);
            assert_eq!(
                t.cmp_paths(&ca, &cb),
                t.path(a).cmp(&t.path(b)),
                "{:?} {:?}",
                String::from_utf8_lossy(&t.path(a)),
                String::from_utf8_lossy(&t.path(b))
            );
        }
    }
}

/// A root that already ends in the separator gets none added after it.
#[test]
fn path_joins_names_with_one_separator() {
    let mut t = Tree::new();
    let top = [b'r', SEP];
    let root = t.new_root(&top, &dir_meta(), true);
    let a = add_dir(&mut t, root, "a");
    let f = add_file(&mut t, a, "f", 1);
    assert_eq!(t.path(root), top);
    assert_eq!(t.path(f), [b'r', SEP, b'a', SEP, b'f']);
    let mut chain = Vec::new();
    t.ancestors(f, &mut chain);
    assert_eq!(chain, [root, a, f]);
    t.ancestors(root, &mut chain);
    assert_eq!(chain, [root], "the previous contents are cleared");
}

#[test]
fn path_buf_joins_the_root_path_and_names() {
    let mut t = Tree::new();
    let root = t.new_root(b"/r", &dir_meta(), true);
    let a = add_dir(&mut t, root, "a");
    let f = add_file(&mut t, a, "f.txt", 1);
    assert_eq!(t.path_buf(f), Path::new("/r").join("a").join("f.txt"));
    assert_eq!(t.path_buf(root), Path::new("/r"));
}

/// Another host's tree joins with that host's separator, whatever this machine uses, and keeps
/// names as stored.
#[test]
fn host_path_joins_with_the_trees_host_separator() {
    let mut t = Tree::new();
    assert_eq!(t.host(), Host::LOCAL);
    t.set_host(Host::Unix);
    let root = t.new_root(b"/home/u", &dir_meta(), true);
    let a = add_dir(&mut t, root, "a\\b");
    let f = add_file(&mut t, a, "f", 1);
    assert_eq!(t.host_path(f).as_bytes(), b"/home/u/a\\b/f");
    assert_eq!(t.host_path(root).as_bytes(), b"/home/u");

    let mut t = Tree::new();
    t.set_host(Host::Windows);
    let root = t.new_root(br"C:\", &dir_meta(), true);
    let a = add_dir(&mut t, root, "Users");
    let f = add_file(&mut t, a, "f", 1);
    let p = t.host_path(f);
    assert_eq!(
        (p.host(), p.as_bytes()),
        (Host::Windows, &br"C:\Users\f"[..])
    );
}
