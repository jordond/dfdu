//! Shared-extent table owned by the applier (`docs/btrfs.md` §3.4).
//!
//! Data that several files reference (snapshots, reflinked copies, deduplicated files) is
//! counted once, at the lowest common ancestor of the files referencing it ("homed" there). A
//! node's disk usage is then what deleting it frees, as long as nothing outside the scan
//! references the data too.
//!
//! Physical ranges are split at every piece boundary, so partial sharing is exact. Compressed
//! extents are keyed by their physical start alone: FIEMAP reports their uncompressed length,
//! which is not a physical range.
//!
//! Only visible nodes count as references. A reference that becomes invisible (its subtree
//! detached) is noticed when its node is freed; until then a home may sit one level too high,
//! never on a node that does not contain it.

use std::collections::{BTreeMap, HashMap};

use dut_tree::{Ix, NodeRef, Tree};

/// `(dev, ino)` of a measured file. Hard links share one entry, positioned at the charged link.
pub type Inode = (u64, u64);
type Fs = u32;

/// What measuring one file found.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Measurement {
    /// Filesystem identity: physical addresses are compared only within one filesystem.
    pub fs: [u8; 16],
    /// Bytes no other file references.
    pub excl: u64,
    /// Some extents are compressed and their on-disk size is not known (`excl` and the shared
    /// lengths count them uncompressed).
    pub compressed: bool,
    /// Shared physical ranges `(start, end)`.
    pub ranges: Vec<(u64, u64)>,
    /// Shared extents identified by their start: compressed extents `(physical start,
    /// uncompressed length)` from FIEMAP, or every shared extent `(disk address, size on disk)`
    /// in exact mode.
    pub points: Vec<(u64, u64)>,
    /// From the extent tree (as root): sizes on disk are exact.
    pub exact: bool,
}

#[derive(Debug)]
struct Seg {
    end: u64,
    refs: Vec<Inode>,
    home: Option<NodeRef>,
    /// Counted at `home` as shared outside the scan (one reference only).
    out: bool,
}

#[derive(Debug)]
struct Point {
    refs: Vec<(Inode, u64)>,
    home: Option<NodeRef>,
    /// Bytes added to `home`: the largest length among the references.
    charged: u64,
    out: bool,
}

#[derive(Debug)]
struct InodeRec {
    fs: Fs,
    pos: NodeRef,
    excl: u64,
    compressed: bool,
    ranges: Vec<(u64, u64)>,
    points: Vec<(u64, u64)>,
}

#[derive(Debug, Default)]
pub struct ExtentTable {
    fs: HashMap<[u8; 16], Fs>,
    segs: BTreeMap<(Fs, u64), Seg>,
    points: HashMap<(Fs, u64), Point>,
    inodes: HashMap<Inode, InodeRec>,
}

impl ExtentTable {
    pub fn is_empty(&self) -> bool {
        self.inodes.is_empty()
    }

    /// The node an inode's measurement is counted at.
    pub fn pos(&self, inode: Inode) -> Option<NodeRef> {
        self.inodes.get(&inode).map(|r| r.pos)
    }

    /// Records the measurement of the file at `node`. A measurement for an inode already
    /// counted at another live node is ignored; one counted at a dead node is replaced.
    pub fn measure(&mut self, tree: &mut Tree, inode: Inode, node: NodeRef, m: Measurement) {
        if let Some(r) = self.inodes.get(&inode) {
            if tree.resolve_visible(r.pos).is_some() {
                return;
            }
            self.remove(tree, inode);
        }
        let Some(ix) = tree.resolve_visible(node) else {
            return;
        };
        let next = self.fs.len() as Fs;
        let fs = *self.fs.entry(m.fs).or_insert(next);
        let ranges = merge(m.ranges);
        let mut points = m.points;
        points.sort();
        points.dedup_by_key(|p| p.0);
        tree.set_measured(ix, m.excl, m.compressed);
        self.inodes.insert(
            inode,
            InodeRec {
                fs,
                pos: node,
                excl: m.excl,
                compressed: m.compressed,
                ranges: ranges.clone(),
                points: points.clone(),
            },
        );
        for (a, b) in ranges {
            self.add_range(tree, fs, a, b, inode, ix);
        }
        for (p, len) in points {
            let pt = self.points.entry((fs, p)).or_insert(Point {
                refs: vec![],
                home: None,
                charged: 0,
                out: false,
            });
            pt.refs.push((inode, len));
            rehome_point(tree, &self.inodes, pt, Some(ix));
        }
    }

    fn add_range(&mut self, tree: &mut Tree, fs: Fs, a: u64, b: u64, inode: Inode, at: Ix) {
        self.split(fs, a);
        self.split(fs, b);
        let starts: Vec<u64> = self
            .segs
            .range((fs, a)..(fs, b))
            .map(|(&(_, s), _)| s)
            .collect();
        let mut cur = a;
        let mut touched = Vec::new();
        for s in starts {
            if cur < s {
                self.segs.insert(
                    (fs, cur),
                    Seg {
                        end: s,
                        refs: vec![],
                        home: None,
                        out: false,
                    },
                );
                touched.push(cur);
            }
            touched.push(s);
            cur = self.segs[&(fs, s)].end;
        }
        if cur < b {
            self.segs.insert(
                (fs, cur),
                Seg {
                    end: b,
                    refs: vec![],
                    home: None,
                    out: false,
                },
            );
            touched.push(cur);
        }
        for s in touched {
            let seg = self.segs.get_mut(&(fs, s)).expect("touched");
            if !seg.refs.contains(&inode) {
                seg.refs.push(inode);
            }
            rehome_seg(tree, &self.inodes, s, seg, Some(at));
        }
    }

    /// Splits the segment containing `x` (if any) at `x`. Both halves keep the home; the bytes
    /// counted there do not change.
    fn split(&mut self, fs: Fs, x: u64) {
        let Some((&(f, s), seg)) = self.segs.range_mut(..(fs, x)).next_back() else {
            return;
        };
        if f != fs || s >= x || x >= seg.end {
            return;
        }
        let tail = Seg {
            end: seg.end,
            refs: seg.refs.clone(),
            home: seg.home,
            out: seg.out,
        };
        seg.end = x;
        self.segs.insert((fs, x), tail);
    }

    /// Forgets an inode (its last link was freed) and re-homes what it referenced.
    pub fn remove(&mut self, tree: &mut Tree, inode: Inode) {
        let Some(rec) = self.inodes.remove(&inode) else {
            return;
        };
        let fs = rec.fs;
        for &(a, b) in &rec.ranges {
            let starts: Vec<u64> = self
                .segs
                .range((fs, a)..(fs, b))
                .map(|(&(_, s), _)| s)
                .collect();
            for s in starts {
                let seg = self.segs.get_mut(&(fs, s)).expect("listed");
                seg.refs.retain(|&r| r != inode);
                rehome_seg(tree, &self.inodes, s, seg, None);
                if seg.refs.is_empty() {
                    self.segs.remove(&(fs, s));
                }
            }
        }
        for &(p, _) in &rec.points {
            let Some(pt) = self.points.get_mut(&(fs, p)) else {
                continue;
            };
            pt.refs.retain(|&(r, _)| r != inode);
            rehome_point(tree, &self.inodes, pt, None);
            if pt.refs.is_empty() {
                self.points.remove(&(fs, p));
            }
        }
    }

    /// Moves an inode's measurement to another of its links (hard-link re-election).
    pub fn move_to(&mut self, tree: &mut Tree, inode: Inode, node: NodeRef) {
        let Some(rec) = self.inodes.get_mut(&inode) else {
            return;
        };
        if rec.pos == node {
            return;
        }
        rec.pos = node;
        let (fs, excl, compressed) = (rec.fs, rec.excl, rec.compressed);
        let (ranges, points) = (rec.ranges.clone(), rec.points.clone());
        if let Some(ix) = tree.resolve(node) {
            tree.set_measured(ix, excl, compressed);
        }
        for (a, b) in ranges {
            let starts: Vec<u64> = self
                .segs
                .range((fs, a)..(fs, b))
                .map(|(&(_, s), _)| s)
                .collect();
            for s in starts {
                let seg = self.segs.get_mut(&(fs, s)).expect("listed");
                rehome_seg(tree, &self.inodes, s, seg, None);
            }
        }
        for (p, _) in points {
            if let Some(pt) = self.points.get_mut(&(fs, p)) {
                rehome_point(tree, &self.inodes, pt, None);
            }
        }
    }

    /// The table's view of one inode, for debugging.
    pub fn describe(&self, tree: &Tree, inode: Inode) -> String {
        let Some(rec) = self.inodes.get(&inode) else {
            return format!("{inode:?}: not in the table");
        };
        let path = |r: NodeRef| {
            tree.resolve(r)
                .map(|ix| String::from_utf8_lossy(&tree.path(ix)).into_owned())
                .unwrap_or_else(|| "freed".into())
        };
        let mut out = format!(
            "{inode:?} at {} excl {} ranges {:?} points {:?}",
            path(rec.pos),
            rec.excl,
            rec.ranges,
            rec.points
        );
        for &(a, b) in &rec.ranges {
            for (&(_, st), seg) in self.segs.range((rec.fs, a)..(rec.fs, b)) {
                out.push_str(&format!(
                    "\n  seg {st}..{} refs {:?} home {:?} out {}",
                    seg.end,
                    seg.refs,
                    seg.home.map(path),
                    seg.out
                ));
            }
        }
        out
    }

    /// Every measured inode is counted at a visible node, and every segment has a home.
    pub fn check_settled(&self, tree: &Tree) -> Result<(), String> {
        for (k, r) in &self.inodes {
            if tree.resolve_visible(r.pos).is_none() {
                return Err(format!(
                    "inode {k:?} is counted at a node that is not visible ({})",
                    if tree.resolve(r.pos).is_some() {
                        "detached"
                    } else {
                        "freed"
                    }
                ));
            }
        }
        Ok(())
    }

    /// Checks every home against a full recomputation, and every node's home bytes against
    /// the table. For the invariant checks of tests and simulation.
    pub fn check(&self, tree: &Tree) -> Result<(), String> {
        let mut expect: HashMap<Ix, u64> = HashMap::new();
        for (&(_, s), seg) in &self.segs {
            if let Some(h) = seg.home.and_then(|h| tree.resolve_visible(h)) {
                let full = lca(tree, &self.inodes, seg.refs.iter().copied());
                if full != Some(h) {
                    // Allowed only while a reference is not visible: its subtree is detached and
                    // not freed yet, or its hard link was freed and the re-election that moves
                    // it to another link is queued. Both end in a full recomputation.
                    let stale = seg.refs.iter().any(|r| {
                        self.inodes
                            .get(r)
                            .is_some_and(|rec| tree.resolve_visible(rec.pos).is_none())
                    });
                    if !stale {
                        let path = |ix: Ix| String::from_utf8_lossy(&tree.path(ix)).into_owned();
                        let refs: Vec<String> = seg
                            .refs
                            .iter()
                            .map(|r| match self.inodes.get(r) {
                                Some(rec) => match tree.resolve(rec.pos) {
                                    Some(ix) => format!(
                                        "{r:?} at {} visible {}",
                                        path(ix),
                                        tree.resolve_visible(rec.pos).is_some()
                                    ),
                                    None => format!("{r:?} at a freed node"),
                                },
                                None => format!("{r:?} not in the table"),
                            })
                            .collect();
                        return Err(format!(
                            "segment {s}..{}: home {}, lowest common ancestor {:?}; refs {refs:?}",
                            seg.end,
                            path(h),
                            full.map(path)
                        ));
                    }
                }
                *expect.entry(h).or_default() += seg.end - s;
            }
        }
        for pt in self.points.values() {
            if let Some(h) = pt.home.and_then(|h| tree.resolve_visible(h)) {
                *expect.entry(h).or_default() += pt.charged;
            }
        }
        for (&ix, &bytes) in &expect {
            if tree.home(ix) != bytes {
                return Err(format!(
                    "{}: home {} bytes, table {bytes}",
                    String::from_utf8_lossy(&tree.path(ix)),
                    tree.home(ix)
                ));
            }
        }
        Ok(())
    }
}

/// Sorted, non-overlapping ranges (a file can reference the same data twice).
fn merge(mut r: Vec<(u64, u64)>) -> Vec<(u64, u64)> {
    r.retain(|&(a, b)| a < b);
    r.sort();
    let mut out: Vec<(u64, u64)> = Vec::with_capacity(r.len());
    for (a, b) in r {
        match out.last_mut() {
            Some(l) if a <= l.1 => l.1 = l.1.max(b),
            _ => out.push((a, b)),
        }
    }
    out
}

/// Re-homes a segment. With `added` (a reference was just added at that node) the new home is
/// the common ancestor of the current home and it; otherwise it is recomputed from every
/// reference.
fn rehome_seg(
    tree: &mut Tree,
    inodes: &HashMap<Inode, InodeRec>,
    start: u64,
    seg: &mut Seg,
    added: Option<Ix>,
) {
    let len = seg.end - start;
    let new = home_for(tree, inodes, seg.home, seg.refs.iter().copied(), added);
    let out = seg.refs.len() == 1;
    move_home(tree, &mut seg.home, &mut seg.out, new, len, len, out);
}

fn rehome_point(
    tree: &mut Tree,
    inodes: &HashMap<Inode, InodeRec>,
    pt: &mut Point,
    added: Option<Ix>,
) {
    let new = home_for(tree, inodes, pt.home, pt.refs.iter().map(|r| r.0), added);
    let len = pt.refs.iter().map(|r| r.1).max().unwrap_or(0);
    let old_len = pt.charged;
    let out = pt.refs.len() == 1;
    move_home(tree, &mut pt.home, &mut pt.out, new, old_len, len, out);
    pt.charged = if pt.home.is_some() { len } else { 0 };
}

fn home_for(
    tree: &Tree,
    inodes: &HashMap<Inode, InodeRec>,
    cur: Option<NodeRef>,
    refs: impl Iterator<Item = Inode>,
    added: Option<Ix>,
) -> Option<NodeRef> {
    let ix = match (added, cur.and_then(|h| tree.resolve_visible(h))) {
        (Some(a), Some(h)) => lca_of(tree, &[h, a]),
        (Some(a), None) if cur.is_none() => Some(a),
        _ => lca(tree, inodes, refs),
    };
    ix.map(|i| tree.node_ref(i))
}

/// Moves `old_len` bytes out of the old home and `new_len` into the new one, with whether
/// they are shared only outside the scan.
#[allow(clippy::too_many_arguments)]
fn move_home(
    tree: &mut Tree,
    home: &mut Option<NodeRef>,
    out: &mut bool,
    new: Option<NodeRef>,
    old_len: u64,
    new_len: u64,
    new_out: bool,
) {
    if *home == new && old_len == new_len && *out == new_out {
        return;
    }
    if let Some(o) = home.and_then(|h| tree.resolve(h)) {
        tree.sub_home(o, old_len, *out);
    }
    if let Some(n) = new.and_then(|h| tree.resolve(h)) {
        tree.add_home(n, new_len, new_out);
    }
    *home = new;
    *out = new_out && new.is_some();
}

/// Lowest common ancestor of the visible positions of `refs`.
fn lca(
    tree: &Tree,
    inodes: &HashMap<Inode, InodeRec>,
    refs: impl Iterator<Item = Inode>,
) -> Option<Ix> {
    let pos: Vec<Ix> = refs
        .filter_map(|r| inodes.get(&r).and_then(|rec| tree.resolve_visible(rec.pos)))
        .collect();
    lca_of(tree, &pos)
}

/// Lowest common ancestor of nodes. Nodes under different visible roots have none; the data is
/// then homed among those under the root of the one with the lowest path.
fn lca_of(tree: &Tree, nodes: &[Ix]) -> Option<Ix> {
    let chains: Vec<Vec<Ix>> = nodes
        .iter()
        .map(|&n| {
            let mut c = vec![n];
            let mut cur = n;
            while let Some(p) = tree.parent(cur) {
                c.push(p);
                cur = p;
            }
            c.reverse();
            c
        })
        .collect();
    let first_root = chains.first()?[0];
    let chains: Vec<&Vec<Ix>> = if chains.iter().all(|c| c[0] == first_root) {
        chains.iter().collect()
    } else {
        let lowest = chains
            .iter()
            .min_by_key(|c| tree.path(*c.last().expect("non-empty")))
            .expect("non-empty")[0];
        chains.iter().filter(|c| c[0] == lowest).collect()
    };
    // The deepest level at which every chain has the same node.
    let mut depth = 0;
    while let Some(&x) = chains[0].get(depth)
        && chains[1..].iter().all(|c| c.get(depth) == Some(&x))
    {
        depth += 1;
    }
    Some(chains[0][depth - 1])
}

#[cfg(test)]
mod tests {
    use super::*;
    use dut_tree::{Kind, Meta, Totals};

    fn meta(kind: Kind, size: u64, ino: u64) -> Meta {
        Meta {
            kind,
            apparent: size,
            alloc: size,
            mtime: 0,
            dev: 1,
            ino,
            nlink: 1,
            mount: 0,
        }
    }

    /// /r/{a/{x,y}, b/{z}} with 100-byte files; returns (tree, root, [x, y, z]).
    fn tree() -> (Tree, Ix, [Ix; 3]) {
        let mut t = Tree::new();
        let r = t.new_root(b"/r", &meta(Kind::Dir, 0, 1), true);
        let a = t.add_dir(r, b"a", &meta(Kind::Dir, 0, 2));
        let b = t.add_dir(r, b"b", &meta(Kind::Dir, 0, 3));
        t.propagate_add(r, &(t.own(a)));
        t.propagate_add(r, &(t.own(b)));
        let f = |t: &mut Tree, d: Ix, n: &[u8], ino| {
            let x = t.add_file(d, n, &meta(Kind::File, 100, ino), false, None);
            let own = t.own(x);
            t.propagate_add(d, &own);
            x
        };
        let x = f(&mut t, a, b"x", 10);
        let y = f(&mut t, a, b"y", 11);
        let z = f(&mut t, b, b"z", 12);
        (t, r, [x, y, z])
    }

    fn m(excl: u64, ranges: &[(u64, u64)]) -> Measurement {
        Measurement {
            excl,
            ranges: ranges.to_vec(),
            ..Default::default()
        }
    }

    fn alloc(t: &Tree, ix: Ix) -> u64 {
        t.total(ix).alloc
    }

    #[test]
    fn shared_data_is_counted_once_at_the_common_ancestor() {
        let (mut t, r, [x, y, z]) = tree();
        let mut e = ExtentTable::default();
        // 100-byte files: x is all shared; y shares [0, 60) with it, z shares [40, 100).
        let n = t.node_ref(x);
        e.measure(&mut t, (1, 10), n, m(0, &[(0, 100)]));
        assert_eq!(alloc(&t, x), 100, "alone, x holds its shared bytes");
        let n = t.node_ref(y);
        e.measure(&mut t, (1, 11), n, m(40, &[(0, 60)]));
        let n = t.node_ref(z);
        e.measure(&mut t, (1, 12), n, m(40, &[(40, 100)]));
        let a = t.parent(x).unwrap();
        let b = t.parent(z).unwrap();
        // [0, 40) x+y -> a; [40, 60) x+y+z -> r; [60, 100) x+z -> r.
        assert_eq!(t.home(a), 40);
        assert_eq!(t.home(r), 60);
        assert_eq!((alloc(&t, x), alloc(&t, y), alloc(&t, z)), (0, 40, 40));
        assert_eq!(alloc(&t, a), 80);
        assert_eq!(alloc(&t, b), 40);
        assert_eq!(alloc(&t, r), 180, "every byte once");
        assert_eq!(t.total(r).referenced, 300);
        e.check(&t).unwrap();
        t.check(&[]).unwrap();

        // Deleting b: what z shared moves down to the files still referencing it.
        t.detach(b);
        let mut freed = vec![];
        while !t.free_step(100, |f| {
            assert!(f.measured);
            freed.push((f.dev, f.ino));
        }) {}
        for i in freed {
            e.remove(&mut t, i);
        }
        // [40, 60) x+y -> a; [60, 100) x alone.
        assert_eq!(t.home(a), 60);
        assert_eq!(t.home(r), 0);
        assert_eq!(alloc(&t, x), 40);
        assert_eq!(alloc(&t, r), 140);
        e.check(&t).unwrap();
        t.check(&[]).unwrap();
    }

    #[test]
    fn order_does_not_matter() {
        let orders = [[0, 1, 2], [2, 1, 0], [1, 2, 0]];
        let mut results: Vec<Vec<Totals>> = vec![];
        for o in orders {
            let (mut t, r, files) = tree();
            let mut e = ExtentTable::default();
            let ms = [
                m(1, &[(0, 50), (70, 90)]),
                m(2, &[(10, 80)]),
                m(3, &[(20, 30), (85, 120)]),
            ];
            for &i in &o {
                let n = t.node_ref(files[i]);
                e.measure(&mut t, (1, 10 + i as u64), n, ms[i].clone());
            }
            e.check(&t).unwrap();
            t.check(&[]).unwrap();
            let a = t.parent(files[0]).unwrap();
            let b = t.parent(files[2]).unwrap();
            results.push(
                [r, a, b, files[0], files[1], files[2]]
                    .map(|n| t.total(n))
                    .to_vec(),
            );
        }
        assert!(results.windows(2).all(|w| w[0] == w[1]), "{results:#?}");
    }

    #[test]
    fn compressed_extents_match_by_start() {
        let (mut t, r, [x, _, z]) = tree();
        let mut e = ExtentTable::default();
        let c = |len| Measurement {
            compressed: true,
            points: vec![(5000, len)],
            ..Default::default()
        };
        let n = t.node_ref(x);
        e.measure(&mut t, (1, 10), n, c(128));
        let n = t.node_ref(z);
        e.measure(&mut t, (1, 12), n, c(64));
        assert_eq!(t.home(r), 128);
        // The compressed extent once, plus y, which is not measured.
        assert_eq!(alloc(&t, r), 128 + 100);
        e.remove(&mut t, (1, 10));
        assert_eq!(t.home(r), 0);
        assert_eq!(t.home(z), 64);
        e.check(&t).unwrap();
        t.check(&[]).unwrap();
    }
}
