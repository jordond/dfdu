use dfdu_tree::{Kind, Meta, NodeFlags};

use super::*;
use crate::test_support::meta;

/// The inode every link of these tests is a link of.
const KEY: (u64, u64) = (1, 50);

/// `/r` with the directories `a` and `b`.
fn two_dirs() -> (Tree, Ix, Ix) {
    let mut t = Tree::new();
    let r = t.new_root(b"/r", &meta(Kind::Dir, 0, 1), true);
    let a = t.add_dir(r, b"a", &meta(Kind::Dir, 0, 2));
    let b = t.add_dir(r, b"b", &meta(Kind::Dir, 0, 3));
    for d in [a, b] {
        let own = t.own(d);
        t.propagate_add(r, &own);
    }
    (t, a, b)
}

/// Adds a link of `key` named `name` under `parent`, as a scan in `scope` finds it.
fn link(
    t: &mut Tree,
    links: &mut LinkTable,
    key: (u64, u64),
    scope: Scope,
    parent: Ix,
    name: &str,
) -> NodeRef {
    let m = Meta {
        nlink: 2,
        ..meta(Kind::File, 100, key.1)
    };
    let mut node = None;
    links.add_scanned(key, 2, scope, |charged| {
        let ix = t.add_file(parent, name.as_bytes(), &m, charged, None);
        let own = t.own(ix);
        t.propagate_add(parent, &own);
        node = Some(ix);
        t.node_ref(ix)
    });
    node.map(|ix| t.node_ref(ix)).expect("made")
}

fn is_charged(t: &Tree, n: NodeRef) -> bool {
    t.resolve(n)
        .is_some_and(|ix| t.flags(ix).has(NodeFlags::CHARGED))
}

/// Detaches and frees the node `n`, as freeing a subtree does.
fn free(t: &mut Tree, n: NodeRef) {
    let ix = t.resolve(n).expect("live");
    t.detach(ix);
    t.free_step(usize::MAX, |_| {});
    assert!(t.is_freed(n));
}

#[test]
fn a_small_list_grows_past_two_and_shrinks_back() {
    let mut s = Small::<i32>::default();
    assert!(matches!(s, Small::Empty));
    for x in 1..=4 {
        s.push(x);
    }
    assert!(matches!(s, Small::Many(_)));
    assert_eq!(&s[..], [1, 2, 3, 4]);
    s.retain(|x| x % 2 == 0);
    assert!(matches!(s, Small::Two([2, 4])));
    assert_eq!(s.swap_remove(0), 2);
    assert!(matches!(s, Small::One([4])));
    s[0] = 5;
    let mut more = Small::default();
    more.push(6);
    more.push(7);
    s.extend(more);
    assert_eq!(s.into_vec(), [5, 6, 7]);
}

/// The first link a scan finds of an inode is charged in its scope, and only that one; each
/// scope charges its own.
#[test]
fn the_first_link_found_in_a_scope_is_charged() {
    let (mut tree, dir_a, dir_b) = two_dirs();
    let mut links = LinkTable::default();
    assert!(!links.has(KEY));
    assert_eq!(links.nlink(KEY), 0);
    let x = link(&mut tree, &mut links, KEY, VISIBLE, dir_b, "x");
    let y = link(&mut tree, &mut links, KEY, VISIBLE, dir_a, "y");
    let z = link(&mut tree, &mut links, KEY, 7, dir_a, "z");
    assert!(links.has(KEY));
    assert_eq!(links.nlink(KEY), 2);
    assert_eq!(links.charged(KEY, VISIBLE), Some(x));
    assert_eq!(links.charged(KEY, 7), Some(z));
    assert_eq!(links.charged(KEY, 8), None);
    assert!(is_charged(&tree, x) && !is_charged(&tree, y) && is_charged(&tree, z));
    assert_eq!(links.shared_keys(VISIBLE), [KEY]);
    assert_eq!(links.shared_keys(7), Vec::<(u64, u64)>::new());
    assert_eq!(links.keys_in(VISIBLE), [KEY]);
    assert_eq!(links.keys_in(7), [KEY]);
}

/// Re-electing charges the link with the lowest path and takes the charge off the old one.
#[test]
fn reelecting_charges_the_lowest_path() {
    let (mut tree, dir_a, dir_b) = two_dirs();
    let mut links = LinkTable::default();
    let x = link(&mut tree, &mut links, KEY, VISIBLE, dir_b, "x");
    let y = link(&mut tree, &mut links, KEY, VISIBLE, dir_a, "y");
    assert_eq!(links.reelect(&mut tree, KEY, VISIBLE), (true, 2));
    assert_eq!(links.charged(KEY, VISIBLE), Some(y));
    assert!(is_charged(&tree, y) && !is_charged(&tree, x));
    assert_eq!(links.reelect(&mut tree, KEY, VISIBLE), (false, 2));
    assert_eq!(links.reelect(&mut tree, KEY, 7), (false, 0));
    assert_eq!(links.reelect(&mut tree, (9, 9), VISIBLE), (false, 0));
}

/// Freeing the charged link asks for a re-election in its scope; once half the links are
/// freed the inode is pruned, and with none left it is forgotten.
#[test]
fn freed_links_are_reelected_around_and_pruned() {
    let (mut tree, dir_a, dir_b) = two_dirs();
    let mut links = LinkTable::default();
    let x = link(&mut tree, &mut links, KEY, VISIBLE, dir_a, "x");
    let y = link(&mut tree, &mut links, KEY, VISIBLE, dir_b, "y");
    free(&mut tree, x);
    assert_eq!(links.remove(KEY, x), (Some(VISIBLE), true));
    assert_eq!(links.charged(KEY, VISIBLE), None);
    assert_eq!(links.prune(&tree, KEY), 2);
    assert_eq!(links.shared_keys(VISIBLE), Vec::<(u64, u64)>::new());
    assert_eq!(links.reelect(&mut tree, KEY, VISIBLE), (true, 1));
    assert_eq!(links.charged(KEY, VISIBLE), Some(y));
    assert!(is_charged(&tree, y));

    free(&mut tree, y);
    assert_eq!(links.remove(KEY, y), (Some(VISIBLE), true));
    assert_eq!(links.prune(&tree, KEY), 1);
    assert!(!links.has(KEY));
    assert_eq!(links.remove(KEY, y), (None, false));
    assert_eq!(links.prune(&tree, KEY), 0);
}

/// Merging a rescan's links into the visible scope keeps one charged link: the moved one is
/// uncharged when both scopes had one, and becomes the charged one when only it was.
#[test]
fn merged_links_keep_one_charged() {
    let (mut tree, dir_a, dir_b) = two_dirs();
    let mut links = LinkTable::default();
    let x = link(&mut tree, &mut links, KEY, VISIBLE, dir_b, "x");
    let z = link(&mut tree, &mut links, KEY, 7, dir_a, "z");
    assert_eq!(links.merge_key(&mut tree, KEY, 7, VISIBLE), (true, 1));
    assert!(is_charged(&tree, x) && !is_charged(&tree, z));
    assert_eq!(links.keys_in(7), Vec::<(u64, u64)>::new());
    assert_eq!(links.shared_keys(VISIBLE), [KEY]);
    assert_eq!(links.reelect(&mut tree, KEY, VISIBLE), (true, 2));
    assert!(is_charged(&tree, z) && !is_charged(&tree, x));

    let other = (1, 60);
    let w = link(&mut tree, &mut links, other, 7, dir_a, "w");
    assert_eq!(links.merge_key(&mut tree, other, 7, VISIBLE), (false, 1));
    assert_eq!(links.charged(other, VISIBLE), Some(w));
    assert!(is_charged(&tree, w));
    assert_eq!(links.merge_key(&mut tree, other, 7, VISIBLE), (false, 0));
    assert_eq!(links.merge_key(&mut tree, (9, 9), 7, VISIBLE), (false, 0));
}
