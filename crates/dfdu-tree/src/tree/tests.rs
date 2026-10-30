use super::test_util::{add_dir, add_file, dir_meta};
use super::*;

#[test]
fn leaves_are_every_non_directory_below() {
    let mut t = Tree::new();
    let r = t.new_root(b"/r", &dir_meta(), true);
    let a = add_dir(&mut t, r, "a");
    let f1 = add_file(&mut t, r, "f1", 1);
    let f2 = add_file(&mut t, a, "f2", 2);
    let empty = add_dir(&mut t, a, "empty");
    let mut seen = Vec::new();
    t.for_each_leaf(r, |ix| seen.push(ix));
    seen.sort();
    let mut want = vec![f1, f2];
    want.sort();
    assert_eq!(seen, want);
    let mut none = Vec::new();
    t.for_each_leaf(empty, |ix| none.push(ix));
    assert_eq!(none, Vec::<Ix>::new());
    // A leaf is its own only leaf.
    let mut one = Vec::new();
    t.for_each_leaf(f2, |ix| one.push(ix));
    assert_eq!(one, [f2]);
}
