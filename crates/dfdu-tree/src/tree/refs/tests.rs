use super::super::test_util::{add_dir, add_file, dir_meta};
use super::*;

#[test]
fn a_reference_survives_its_bits() {
    for r in [
        NodeRef {
            index: 0,
            generation: 0,
        },
        NodeRef {
            index: 7,
            generation: 3,
        },
        NodeRef {
            index: u32::MAX,
            generation: u32::MAX,
        },
    ] {
        assert_eq!(NodeRef::from_bits(r.to_bits()), r);
    }
    // Any number is a reference; one to no node resolves to nothing.
    assert_eq!(Tree::new().resolve(NodeRef::from_bits(u64::MAX)), None);
}

/// A node in a tree that is not shown resolves, but not as visible; once detached, neither.
#[test]
fn visible_is_below_the_visible_root() {
    let mut t = Tree::new();
    let shown = t.new_root(b"/shown", &dir_meta(), true);
    let hidden = t.new_root(b"/hidden", &dir_meta(), false);
    let a = add_file(&mut t, shown, "a", 1);
    let b = add_file(&mut t, hidden, "b", 1);
    let (ra, rb) = (t.node_ref(a), t.node_ref(b));
    assert_eq!((t.resolve(ra), t.resolve_visible(ra)), (Some(a), Some(a)));
    assert_eq!((t.resolve(rb), t.resolve_visible(rb)), (Some(b), None));
    assert!(t.is_visible(a) && !t.is_visible(b));
    t.detach(a);
    assert_eq!((t.resolve(ra), t.resolve_visible(ra)), (None, None));
    assert!(!t.is_visible(a));
    assert!(!t.is_freed(ra), "detached, not freed yet");
    t.free_step(usize::MAX, |_| {});
    assert!(t.is_freed(ra));
}

#[test]
fn a_cut_directory_hides_what_is_below_it() {
    let mut t = Tree::new();
    let r = t.new_root(b"/r", &dir_meta(), true);
    let d = add_dir(&mut t, r, "d");
    let f = add_file(&mut t, d, "f", 1);
    let rf = t.node_ref(f);
    assert_eq!(t.resolve_uncut(rf), Some(f));
    t.dir_mut(d).unwrap().cut = true;
    assert_eq!(t.resolve_uncut(rf), None);
    assert_eq!(
        t.resolve_uncut(t.node_ref(d)),
        None,
        "the cut directory itself"
    );
    assert_eq!(t.resolve_uncut(t.node_ref(r)), Some(r));
    assert_eq!(t.resolve(rf), Some(f), "cut is not gone");
}

#[test]
fn ancestors_come_topmost_first() {
    let mut t = Tree::new();
    let r = t.new_root(b"/r", &dir_meta(), true);
    let d = add_dir(&mut t, r, "d");
    let f = add_file(&mut t, d, "f", 1);
    let mut out = vec![Ix(99)];
    assert_eq!(t.resolve_ancestors(t.node_ref(f), &mut out), Some(f));
    assert_eq!(out, [r, d, f]);
    let rd = t.node_ref(d);
    t.detach(d);
    assert_eq!(t.resolve_ancestors(rd, &mut out), None);
    assert!(out.is_empty(), "nothing left from a failed walk");
}
