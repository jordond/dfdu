use dfdu_scan::PortableBackend;
use dfdu_tree::{ErrKind, Kind};

use super::*;
use crate::applier::test_util::{job_class, project};
use crate::test_support::meta;

/// Classifies the whole tree from its root, `units` entries a step; returns the directories
/// in the order they were reached, and the number of steps.
fn classify(t: &mut Tree, units: usize) -> (Vec<Ix>, usize) {
    let opts = job_class();
    let root = t.roots().next().expect("root");
    let mut c = ClassifyLoaded::new(t, root, &opts);
    let (mut seen, mut steps) = (Vec::new(), 0);
    loop {
        steps += 1;
        let backend = PortableBackend::default();
        if c.step(t, &opts, &backend, units, |_, d| seen.push(d)) {
            return (seen, steps);
        }
    }
}

/// A loaded tree gets the classes a scan of it would give: `node_modules` is a match beside a
/// `package.json`, and not without one.
#[test]
fn a_loaded_tree_gets_the_classes_a_scan_gives() {
    let rule = job_class().rules.index("node.node_modules").expect("rule");
    let (mut t, [r, p, nm]) = project(true);
    let (seen, _) = classify(&mut t, usize::MAX);
    assert_eq!(seen[..2], [r, p]);
    assert_eq!(t.class(nm).map(|c| c.rule), Some(rule));
    assert_eq!(t.class(p), None);

    let (mut t, [_, _, nm]) = project(false);
    classify(&mut t, usize::MAX);
    assert_eq!(t.class(nm), None);
}

/// A step classifies whole directories until their entries reach its units, so a small
/// budget takes a step per directory. An entry that could not be stat'ed is only a name to
/// the rules, and gets no class.
#[test]
fn a_step_stops_at_its_units_after_a_whole_directory() {
    let (mut t, [r, p, nm]) = project(true);
    let bad = t.add_file(p, b"bad", &meta(Kind::File, 0, 5), false, None);
    t.set_error(bad, ErrKind::PermissionDenied);
    let (seen, steps) = classify(&mut t, 1);
    assert_eq!(seen, [r, p, nm]);
    assert_eq!(steps, 3);
    let (all, steps) = classify(&mut t, usize::MAX);
    assert_eq!((all, steps), (seen, 1));
    assert_eq!(t.class(bad), None);
}

/// A directory taken out of the tree between steps is passed over, with what is below it.
#[test]
fn a_directory_no_longer_visible_is_passed_over() {
    let (mut t, [r, p, nm]) = project(true);
    let below = t.add_dir(nm, b"x", &meta(Kind::Dir, 0, 6));
    let opts = job_class();
    let backend = PortableBackend::default();
    let mut c = ClassifyLoaded::new(&t, r, &opts);
    let mut seen = Vec::new();
    assert!(!c.step(&mut t, &opts, &backend, 1, |_, d| seen.push(d)));
    t.detach(p);
    assert!(c.step(&mut t, &opts, &backend, usize::MAX, |_, d| seen.push(d)));
    assert_eq!(seen, [r]);
    assert!(t.resolve_visible(t.node_ref(below)).is_none());
}
