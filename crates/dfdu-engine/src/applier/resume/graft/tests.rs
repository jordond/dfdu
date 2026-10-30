use dfdu_scan::PortableBackend;
use dfdu_tree::Kind;

use super::*;
use crate::test_util::meta;

type A = Applier<PortableBackend>;

/// Directory `name` of inode `ino` added to `parent`, its totals with it.
fn dir(t: &mut Tree, parent: Ix, name: &str, ino: u64) -> Ix {
    let d = t.add_dir(parent, name.as_bytes(), &meta(Kind::Dir, 0, ino));
    let own = t.own(d);
    t.propagate_add(parent, &own);
    d
}

/// The visible /r/{t/{k/{x}, kk/}, o/}, the rescan of `t` and its shadow holding placeholders
/// `k` (`k`'s inode), `k2` (`kk`'s), `z` (an inode not kept) and `o` (`o`'s, outside `t`), and
/// what the rescan keeps: `k`, `kk` and `o`. Returns the model, the kept, `t`, `k` and the
/// placeholders.
fn grafting() -> (Model, Kept, Ix, Ix, [NodeRef; 4]) {
    let mut m = Model::default();
    let tr = &mut m.tree;
    let r = tr.new_root(b"/r", &meta(Kind::Dir, 0, 1), true);
    let t = dir(tr, r, "t", 2);
    let k = dir(tr, t, "k", 3);
    let x = tr.add_file(k, b"x", &meta(Kind::File, 10, 4), false, None);
    let own = tr.own(x);
    tr.propagate_add(k, &own);
    let kk = dir(tr, t, "kk", 7);
    let o = dir(tr, r, "o", 5);
    let shadow = tr.new_root(b"t", &meta(Kind::Dir, 0, 2), false);
    let placeholders = [("k", 3), ("k2", 7), ("z", 9), ("o", 5)]
        .map(|(n, ino)| dir(tr, shadow, n, ino))
        .map(|p| tr.node_ref(p));
    let mut kept = Kept::default();
    for d in [k, kk, o] {
        kept.nodes.insert(tr.dev_ino(d), (tr.node_ref(d), 0));
    }
    kept.placeholders = placeholders.to_vec();
    (m, kept, t, k, placeholders)
}

/// Each placeholder is matched to the kept directory of its inode below the rescan's target:
/// where it was, under another name, or none (a placeholder whose directory is not kept, or is
/// outside the target). The graft puts the found ones in place; what moved or was not found
/// is rescanned, the lost placeholders marked cancelled.
#[test]
fn kept_directories_take_the_place_of_their_placeholders() {
    let (mut m, mut kept, t, k, [pk, pk2, pz, po]) = grafting();
    assert!(
        A::find_kept(&m, &mut kept, t, None),
        "all found in one step"
    );
    let kk = m.tree.node_ref(m.tree.children(t).nth(1).unwrap());
    let found: Vec<_> = kept
        .finding
        .found
        .iter()
        .map(|f| match f {
            Found::Lost(p) => ("lost", *p, None),
            Found::Moved(p, r) => ("moved", *p, Some(*r)),
            Found::Same(p, r, hits) => {
                assert!(hits.is_none(), "the rescan does not classify");
                ("same", *p, Some(*r))
            }
        })
        .collect();
    let kr = m.tree.node_ref(k);
    assert_eq!(
        found,
        [
            ("same", pk, Some(kr)),
            ("moved", pk2, Some(kk)),
            ("lost", pz, None),
            ("lost", po, None),
        ]
    );
    let mut g = A::graft_kept(&mut m, &mut kept, t, None);
    assert!(!g.rehome);
    assert_eq!(g.again, [kk, pz, po]);
    for p in [pz, po] {
        let d = m.tree.dir(m.tree.resolve(p).unwrap()).unwrap();
        assert_eq!(d.state, DirState::Cancelled);
    }
    let shadow = m.tree.parent(m.tree.resolve(kr).unwrap()).unwrap();
    assert!(!m.tree.is_visible(shadow), "k is in the shadow now");
    assert!(
        m.tree
            .resolve(pk)
            .is_none_or(|p| m.tree.parent(p).is_none())
    );
    assert_eq!(g.step(&m.tree), (vec![kk, pz, po], true));
}

/// A kept directory found twice takes the place of the first placeholder only.
#[test]
fn a_kept_directory_takes_one_place() {
    let (mut m, mut kept, t, _, [pk, ..]) = grafting();
    let shadow = m.tree.parent(m.tree.resolve(pk).unwrap()).unwrap();
    let again = dir(&mut m.tree, shadow, "k", 3);
    kept.placeholders = vec![pk, m.tree.node_ref(again)];
    assert!(A::find_kept(&m, &mut kept, t, None));
    assert!(matches!(
        kept.finding.found[..],
        [Found::Same(..), Found::Lost(_)]
    ));
}

/// Shared extents homed between a kept directory and the rescan's target are homed again
/// once it has swapped in.
#[test]
fn extents_homed_above_a_kept_directory_are_homed_again() {
    let (mut m, mut kept, t, k, _) = grafting();
    m.tree.add_shared(t, 100, false);
    assert!(homed_above(&m.tree, &[(k, k)], t));
    let r = m.tree.parent(t).unwrap();
    assert!(
        !homed_above(&m.tree, &[(r, r)], t),
        "nothing above the root"
    );
    assert!(A::find_kept(&m, &mut kept, t, None));
    assert!(A::graft_kept(&mut m, &mut kept, t, None).rehome);
}

/// What a graft leaves to rescan goes a step at a time, the last step saying so.
#[test]
fn grafted_rescans_go_a_step_at_a_time() {
    let refs: Vec<NodeRef> = (0..RESCANS_PER_STEP as u64 + 4)
        .map(NodeRef::from_bits)
        .collect();
    let mut g = Grafted {
        class: None,
        again: refs.clone(),
        check: Vec::new(),
        rehome: false,
    };
    let tree = Tree::new();
    let (first, last) = g.step(&tree);
    assert_eq!((first.len(), last), (RESCANS_PER_STEP, false));
    let (rest, last) = g.step(&tree);
    assert_eq!((rest.len(), last), (4, true));
    // Without a class, nothing kept is judged again.
    g.check = vec![(refs[0], Vec::new()); PER_STEP + 1];
    assert_eq!(g.step(&tree), (vec![], false));
    assert_eq!(g.step(&tree), (vec![], true));
}

/// Two classes are the same match when the rule, the safety and the degradation are.
#[test]
fn a_verdict_leaves_out_the_pools_flags() {
    let c = Class {
        rule: RuleIx(3),
        safety: Safety::Review,
        degraded: Some(1),
        flags: 0,
    };
    assert_eq!(
        verdict(c),
        verdict(Class {
            flags: Class::CHECKED,
            ..c
        })
    );
    assert_ne!(
        verdict(c),
        verdict(Class {
            degraded: None,
            ..c
        })
    );
}
