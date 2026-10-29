use dfdu_tree::Meta;

use super::super::{Spec, SpecConfig};
use super::*;

fn node(name: &str, kind: Kind, dev: u64) -> SpecNode {
    let meta = Meta {
        kind,
        apparent: 4096,
        alloc: 4096,
        mtime: 1,
        dev,
        ino: 1,
        nlink: 1,
        mount: 0,
    };
    SpecNode::new(name.as_bytes().to_vec(), meta)
}

/// `r/{a/{x}, b}`: 0 is `r`, 1 `a`, 2 `x`, 3 `b`.
fn small() -> Vec<SpecNode> {
    let mut nodes = vec![
        node("r", Kind::Dir, 1),
        node("a", Kind::Dir, 1),
        node("x", Kind::File, 1),
        node("b", Kind::Dir, 2),
    ];
    nodes[0].children = vec![1, 3];
    nodes[1].children = vec![2];
    nodes
}

#[test]
fn copies_are_new_nodes_with_the_same_contents() {
    let mut nodes = small();
    let copy = copy_subtree(&mut nodes, 1);
    assert_eq!(nodes.len(), 6);
    let c = &nodes[copy as usize];
    assert_eq!((&c.name[..], c.children.len()), (&b"a"[..], 1));
    let x = c.children[0];
    assert!(x != 2 && &nodes[x as usize].name[..] == b"x");
    assert_eq!(nodes[1].children, [2], "the original is left alone");
    set_dev(&mut nodes, copy, 9);
    assert_eq!(
        (nodes[copy as usize].meta.dev, nodes[x as usize].meta.dev),
        (9, 9)
    );
    assert_eq!(nodes[2].meta.dev, 1);
}

#[test]
fn subtrees_know_what_they_hold() {
    let nodes = small();
    assert!(contains(&nodes, 0, 2) && contains(&nodes, 1, 2) && contains(&nodes, 1, 1));
    assert!(!contains(&nodes, 1, 3) && !contains(&nodes, 2, 1));
    assert!(single_device(&nodes, 1));
    assert!(!single_device(&nodes, 0), "b is on another device");
}

/// Two extents on the same filesystem are shared when they overlap (uncompressed) or start at
/// the same place (compressed); a separate one is shared only by chance.
#[test]
fn overlapping_extents_are_marked_shared() {
    let piece = |phys, len, encoded| SimPiece {
        phys,
        len,
        encoded,
        shared: false,
    };
    let mut nodes = vec![node("r", Kind::Dir, 1)];
    let files = [
        vec![piece(4096, 8192, false)],
        vec![piece(8192, 4096, false)],
        vec![piece(1 << 20, 4096, true)],
        vec![piece(1 << 20, 8192, true)],
    ];
    for (i, pieces) in files.into_iter().enumerate() {
        let mut f = node(&format!("f{i}"), Kind::File, 1);
        f.meta.ino = 10 + i as u64;
        f.pieces = pieces;
        nodes.push(f);
    }
    nodes[0].children = vec![1, 2, 3, 4];
    mark_shared(&mut Rng::new(5), &mut nodes);
    assert!(nodes[1..].iter().all(|n| n.pieces[0].shared));
    // Not reachable from the root: it does not exist, so it shares nothing.
    nodes[0].children = vec![1, 3];
    for n in &mut nodes {
        for p in &mut n.pieces {
            p.shared = false;
        }
    }
    let mut seeds_unshared = 0;
    for seed in 0..20 {
        let mut ns = nodes.clone();
        mark_shared(&mut Rng::new(seed), &mut ns);
        seeds_unshared += u32::from(!ns[1].pieces[0].shared);
    }
    assert!(seeds_unshared > 0, "only shared by chance");
}

/// In generated specs only files on filesystems that share extents have extents, in whole
/// blocks, and hard links have the same ones.
#[test]
fn generated_extents_are_whole_blocks_on_sharing_filesystems() {
    let cfg = SpecConfig::default();
    let mut with = 0;
    for seed in 0..40 {
        let spec = Spec::generate(&mut Rng::new(seed), &cfg, 3);
        for n in &spec.nodes {
            if n.pieces.is_empty() {
                continue;
            }
            with += 1;
            assert_eq!(n.meta.kind, Kind::File);
            assert!(spec.dev_fs[&n.meta.dev].1, "seed {seed}");
            for p in &n.pieces {
                assert!(p.len > 0 && p.len.is_multiple_of(4096) && p.phys.is_multiple_of(4096));
            }
        }
    }
    assert!(with > 0);
    let off = SpecConfig {
        extents: false,
        ..SpecConfig::default()
    };
    let spec = Spec::generate(&mut Rng::new(1), &off, 3);
    assert!(spec.nodes.iter().all(|n| n.pieces.is_empty()));
}
