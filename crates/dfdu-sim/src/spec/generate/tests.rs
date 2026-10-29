use std::collections::HashSet;

use super::*;

const BLOCK: usize = 3;

fn generate(seed: u64, cfg: &SpecConfig) -> Spec {
    Spec::generate(&mut Rng::new(seed), cfg, BLOCK)
}

/// Nodes reachable from the root: snapshots replace a subvolume's children, which leaves the
/// old ones in `nodes` but out of the tree.
fn reachable(spec: &Spec) -> Vec<u32> {
    let mut out = vec![];
    let mut stack = vec![ROOT];
    while let Some(n) = stack.pop() {
        out.push(n);
        stack.extend(&spec.nodes[n as usize].children);
    }
    out
}

fn summary(spec: &Spec) -> String {
    let mut fs: Vec<_> = spec.dev_fs.iter().collect();
    fs.sort();
    format!("{:?} {:?} {fs:?}", spec.nodes, spec.parent)
}

#[test]
fn a_seed_gives_the_same_spec() {
    let cfg = SpecConfig::default();
    for seed in 0..20 {
        assert_eq!(
            summary(&generate(seed, &cfg)),
            summary(&generate(seed, &cfg))
        );
    }
    assert_ne!(summary(&generate(1, &cfg)), summary(&generate(2, &cfg)));
}

/// Each reachable node is listed once, under the parent the table names, with a name unique
/// in its directory and a device the spec knows.
#[test]
fn the_tree_is_well_formed() {
    let cfg = SpecConfig::default();
    for seed in 0..60 {
        let spec = generate(seed, &cfg);
        let nodes = reachable(&spec);
        let mut seen = HashSet::new();
        for &n in &nodes {
            assert!(seen.insert(n), "seed {seed}: node {n} listed twice");
            let node = &spec.nodes[n as usize];
            assert!(spec.dev_fs.contains_key(&node.meta.dev), "seed {seed}");
            let mut names = HashSet::new();
            for &c in &node.children {
                assert_eq!(spec.parent[c as usize], n, "seed {seed}");
                assert!(
                    names.insert(&spec.nodes[c as usize].name),
                    "seed {seed}: a name twice in {n}"
                );
            }
            if node.meta.kind != Kind::Dir {
                assert!(node.children.is_empty());
            }
            if node.subvol.is_some() {
                assert_eq!(node.meta.ino, 256, "seed {seed}");
            }
        }
        assert!(
            spec.deleted
                .iter()
                .all(|d| !d.load(std::sync::atomic::Ordering::SeqCst))
        );
    }
}

/// A fault position is inside its directory's listing.
#[test]
fn faults_fall_inside_the_listing() {
    let cfg = SpecConfig::default();
    let mut faults = 0;
    for seed in 0..60 {
        let spec = generate(seed, &cfg);
        for n in reachable(&spec) {
            let node = &spec.nodes[n as usize];
            let len = node.children.len();
            let last_block = len.saturating_sub(1) / BLOCK;
            let ok = match node.fault {
                Fault::None => continue,
                Fault::Eacces | Fault::PanicOnOpen => true,
                Fault::EioAfter(k) => k <= len,
                Fault::Stall { block, .. } => block <= last_block,
                Fault::PanicAtBlock(b)
                | Fault::StatPanic(b)
                | Fault::StatStall { block: b, .. } => len > 0 && b <= last_block,
            };
            assert!(ok, "seed {seed}: {:?} in a listing of {len}", node.fault);
            faults += 1;
        }
    }
    assert!(faults > 0);
    let none = SpecConfig {
        faults: false,
        ..SpecConfig::default()
    };
    let spec = generate(3, &none);
    assert!(
        spec.nodes
            .iter()
            .all(|n| n.fault == Fault::None && !n.vanished)
    );
}

/// Every name of an inode is the same file: the same sizes, link count, extents and
/// measurability, and a link count of at least its names in the tree.
#[test]
fn hard_links_are_one_file() {
    let cfg = SpecConfig::default();
    let mut linked = 0;
    for seed in 0..60 {
        let spec = generate(seed, &cfg);
        // Second mounts show the same inodes again: they are left out.
        let mut by_inode: HashMap<(u64, u64), Vec<u32>> = HashMap::new();
        let mut stack = vec![ROOT];
        while let Some(n) = stack.pop() {
            let node = &spec.nodes[n as usize];
            if node.dup_mount {
                continue;
            }
            if node.meta.kind == Kind::File {
                by_inode
                    .entry((node.meta.dev, node.meta.ino))
                    .or_default()
                    .push(n);
            }
            stack.extend(&node.children);
        }
        for names in by_inode.values() {
            let first = &spec.nodes[names[0] as usize];
            for &n in &names[1..] {
                let other = &spec.nodes[n as usize];
                assert_eq!(other.meta, first.meta, "seed {seed}");
                assert_eq!(other.pieces, first.pieces, "seed {seed}");
                assert_eq!(other.unmeasurable, first.unmeasurable, "seed {seed}");
            }
            if names.len() > 1 {
                linked += 1;
                assert!(first.meta.nlink >= 2, "seed {seed}");
            }
        }
    }
    assert!(linked > 0);
}

#[test]
fn rule_names_are_given_only_when_asked_for() {
    let plain = SpecConfig::default();
    let named = SpecConfig {
        rule_names: true,
        ..SpecConfig::default()
    };
    let names = |cfg: &SpecConfig| {
        let mut out = HashSet::new();
        for seed in 0..30 {
            let spec = generate(seed, cfg);
            for n in reachable(&spec).into_iter().skip(1) {
                out.insert(String::from_utf8(spec.nodes[n as usize].name.clone()).unwrap());
            }
        }
        out
    };
    let generated = |s: &str| {
        let s = s.strip_suffix("-again").unwrap_or(s);
        s.len() > 1
            && (s.starts_with('d') || s.starts_with('f'))
            && s[1..].bytes().all(|b| b.is_ascii_digit())
    };
    assert!(
        names(&plain).iter().all(|n| generated(n)),
        "{:?}",
        names(&plain)
    );
    let named = names(&named);
    for want in ["target", "Cargo.toml", "CACHEDIR.TAG"] {
        assert!(named.contains(want), "{want}");
    }
}
