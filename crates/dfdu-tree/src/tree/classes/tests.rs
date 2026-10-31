use super::super::test_util::{add_dir, add_file, dir_meta};
use super::*;

fn class(rule: u16) -> Class {
    Class {
        rule: crate::RuleIx(rule),
        safety: crate::Safety::Safe,
        degraded: None,
        flags: crate::ClassFlags::NONE,
    }
}

#[test]
fn a_reused_slot_has_no_class_or_protection() {
    let mut t = Tree::new();
    let root = t.new_root(b"/r", &dir_meta(), true);
    let d = t.add_dir(root, b"target", &dir_meta());
    let f = add_file(&mut t, d, "f", 1);
    let class = Class {
        rule: crate::RuleIx(3),
        safety: crate::Safety::Regenerable,
        degraded: None,
        flags: crate::ClassFlags::NONE,
    };
    t.set_class(d, Some(class));
    t.set_protection(root, Protection::Protected);
    t.set_protection(d, Protection::Excepted);
    assert_eq!(t.class_in_effect(f, |_| false), Some((d, class)));
    assert!(t.is_protected(root) && !t.is_protected(f));
    t.detach(d);
    t.free_step(usize::MAX, |_| {});
    assert_eq!(t.classes().count(), 0);
    let e = t.add_dir(root, b"e", &dir_meta());
    assert!([d.0, f.0].contains(&e.0), "slot reused");
    assert_eq!(t.class(e), None);
    assert!(t.is_protected(e), "only the root's mark applies");
}

/// The nearest match covers a node, unless a match above it stops the descent: then the
/// highest such match does.
#[test]
fn the_nearest_match_is_in_effect_unless_one_above_stops_the_descent() {
    let mut t = Tree::new();
    let root = t.new_root(b"/r", &dir_meta(), true);
    let outer = add_dir(&mut t, root, "outer");
    let mid = add_dir(&mut t, outer, "mid");
    let inner = add_dir(&mut t, mid, "inner");
    let f = add_file(&mut t, inner, "f", 1);
    t.set_class(outer, Some(class(1)));
    t.set_class(inner, Some(class(2)));
    let none = |_: &Class| false;
    assert_eq!(t.class_in_effect(f, none), Some((inner, class(2))));
    assert_eq!(t.class_in_effect(mid, none), Some((outer, class(1))));
    assert_eq!(t.class_in_effect(root, none), None);
    // Rule 1 stops the descent: it covers what is inside it, the nearer match too.
    let one = |c: &Class| c.rule.0 == 1;
    assert_eq!(t.class_in_effect(f, one), Some((outer, class(1))));
    // Both stop it: the highest wins.
    let all = |_: &Class| true;
    assert_eq!(t.class_in_effect(f, all), Some((outer, class(1))));
    // A match below that stops the descent does not lift one above that does not.
    let two = |c: &Class| c.rule.0 == 2;
    assert_eq!(t.class_in_effect(f, two), Some((inner, class(2))));
    // One step down at a time gives the same.
    let below = t.class_below(t.class_in_effect(mid, one), inner, one);
    assert_eq!(below, Some((outer, class(1))));
    assert_eq!(t.class_below(None, inner, one), Some((inner, class(2))));
}

#[test]
fn clearing_a_class_removes_it() {
    let mut t = Tree::new();
    let root = t.new_root(b"/r", &dir_meta(), true);
    let d = add_dir(&mut t, root, "d");
    t.set_class(d, Some(class(1)));
    t.set_class(d, Some(class(2)));
    assert_eq!(t.classes().collect::<Vec<_>>(), [(d, class(2))]);
    t.set_class(d, None);
    assert_eq!(t.class(d), None);
    assert_eq!(t.class_in_effect(d, |_| true), None);
}

/// The nearest mark at or above a node decides: a protected subtree inside an exception
/// inside a protected subtree is protected again.
#[test]
fn the_nearest_protection_mark_decides() {
    let mut t = Tree::new();
    let root = t.new_root(b"/r", &dir_meta(), true);
    let kept = add_dir(&mut t, root, "kept");
    let lifted = add_dir(&mut t, kept, "lifted");
    let again = add_dir(&mut t, lifted, "again");
    let f = add_file(&mut t, again, "f", 1);
    let g = add_file(&mut t, lifted, "g", 1);
    assert!(!t.is_protected(f), "nothing is marked");
    t.set_protection(kept, Protection::Protected);
    t.set_protection(lifted, Protection::Excepted);
    t.set_protection(again, Protection::Protected);
    assert_eq!(
        [kept, lifted, f].map(|ix| t.protection(ix)),
        [
            Protection::Protected,
            Protection::Excepted,
            Protection::None
        ]
    );
    assert!(!t.is_protected(root));
    assert!(t.is_protected(kept));
    assert!(!t.is_protected(lifted) && !t.is_protected(g));
    assert!(t.is_protected(again) && t.is_protected(f));
    let before = t.total(root);
    t.set_protection(again, Protection::None);
    assert!(!t.is_protected(f));
    assert_eq!(t.total(root), before, "marks are not counted in totals");
}
