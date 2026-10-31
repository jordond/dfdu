use super::super::test_util::{add_dir, add_file, dir_meta};
use super::*;

#[test]
fn a_target_is_found_again_by_path_only_with_its_identity() {
    let mut t = Tree::new();
    let r = t.new_root(b"/r", &dir_meta(), true);
    let d = add_dir(&mut t, r, "d");
    let f = add_file(&mut t, d, "f", 100);
    let g = add_file(&mut t, d, "g", 200);
    let (tf, tg) = (Target::of(&t, f), Target::of(&t, g));
    assert_eq!(tf.resolve(&t), Some(f));
    assert_eq!(find_path(&t, &t.host_path(g)), Some(g));
    // Both replaced under the same names: `f` by another file, `g` by the same one.
    t.detach(f);
    t.detach(g);
    let f2 = add_file(&mut t, d, "f", 300);
    let g2 = add_file(&mut t, d, "g", 200);
    assert_eq!(tf.resolve(&t), None);
    assert_eq!(tg.resolve(&t), Some(g2));
    // Without an identity the path is enough.
    let bare = Target {
        ident: None,
        ..tf.clone()
    };
    assert_eq!(bare.resolve(&t), Some(f2));
    assert_eq!(Target::root().resolve(&t), None);
}
