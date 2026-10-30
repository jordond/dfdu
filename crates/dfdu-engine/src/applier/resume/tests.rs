use super::*;
use crate::test_util::meta;
use dfdu_tree::Kind;

/// /r/{a/{b/, f}, c/}; returns the tree and r, a, b, c.
#[expect(
    clippy::many_single_char_names,
    reason = "nodes named as in the fixture's diagram"
)]
fn tree() -> (Tree, [Ix; 4]) {
    let mut t = Tree::new();
    let r = t.new_root(b"/r", &meta(Kind::Dir, 0, 1), true);
    let a = t.add_dir(r, b"a", &meta(Kind::Dir, 0, 2));
    let b = t.add_dir(a, b"b", &meta(Kind::Dir, 0, 3));
    let c = t.add_dir(r, b"c", &meta(Kind::Dir, 0, 4));
    t.add_file(a, b"f", &meta(Kind::File, 0, 5), false, None);
    for d in [r, a, b, c] {
        t.dir_mut(d).unwrap().state = DirState::Complete;
    }
    (t, [r, a, b, c])
}

#[test]
#[expect(
    clippy::many_single_char_names,
    reason = "nodes named as in the fixture's diagram"
)]
fn changed_paths_name_their_directory_or_its_nearest_ancestor() {
    let (t, [r, a, b, c]) = tree();
    let paths = ["a/b", "c", "a/gone/deeper", "a/f", ""].map(PathBuf::from);
    assert_eq!(changed_dirs(&t, r, &paths), [b, c, a, a, r]);
}

#[test]
fn only_the_topmost_changed_directories_are_kept() {
    let (t, [_, a, b, c]) = tree();
    let mut want = vec![a, c];
    want.sort_unstable_by_key(|d| d.0);
    assert_eq!(topmost(&t, vec![b, c, a, b]), want);
}

fn keys(k: &Option<Box<Kept>>) -> Vec<(u64, u64)> {
    let mut v: Vec<(u64, u64)> = k.as_ref().unwrap().nodes.keys().copied().collect();
    v.sort_unstable();
    v
}

/// A change in `b` lists `r`, `a` and `b` again, keeping `c`: the rescan is of the root
/// when it changed, of `b` alone when it did not.
#[test]
fn what_is_listed_again_keeps_the_rest() {
    let (t, [r, a, b, c]) = tree();
    let kept = kept_by_target(&t, r, &[r, b], &[r], &[]);
    assert_eq!(keys(&kept[0]), [t.dev_ino(c)]);
    let kept = kept_by_target(&t, r, &[a], &[a], &[]);
    assert_eq!(keys(&kept[0]), [t.dev_ino(b)]);
    let kept = kept_by_target(&t, r, &[b], &[b], &[]);
    assert!(keys(&kept[0]).is_empty());
    // A mount is scanned in full.
    assert!(kept_by_target(&t, r, &[c], &[c], &[c])[0].is_none());
}

/// A directory whose listing was not read to the end is listed again.
#[test]
fn an_incomplete_directory_is_not_kept() {
    let (mut t, [r, a, _, c]) = tree();
    t.dir_mut(c).unwrap().state = DirState::Partial;
    let kept = kept_by_target(&t, r, &[r], &[r], &[]);
    assert_eq!(keys(&kept[0]), [t.dev_ino(a)]);
}

/// A kept directory goes to its scanner with the mtime it was loaded with.
#[test]
fn a_kept_directory_is_kept_with_its_loaded_mtime() {
    let (t, [r, a, _, c]) = tree();
    let kept = kept_by_target(&t, r, &[r], &[r], &[]);
    let keep = kept[0].as_ref().unwrap().keep();
    let (dev, ino) = t.dev_ino(c);
    assert!(!keep.take(dev, ino, t.mtime(c) + 1), "its entries changed");
    let (dev, ino) = t.dev_ino(a);
    assert!(keep.take(dev, ino, t.mtime(a)));
}
