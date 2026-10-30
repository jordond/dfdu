//! Shared-extent table owned by the applier.
//!
//! Data that several files reference (snapshots, reflinked copies, deduplicated files) is
//! counted once, at the lowest common ancestor of the files referencing it ("homed" there). A
//! node's disk usage is then what deleting it frees, as long as nothing outside the scan
//! references the data too.
//!
//! Physical ranges are split at every piece boundary, so partial sharing is exact. In exact mode
//! (as root) shared extents are keyed by their disk address alone ("points"), with their size
//! on disk.
//!
//! Only visible nodes count as references. A reference that becomes invisible (its subtree
//! detached) is noticed when its node is freed; until then a home may sit one level too high,
//! never on a node that does not contain it.

use std::collections::{BTreeMap, HashMap};
use std::fmt::Write as _;
use std::hash::{BuildHasherDefault, Hasher};

use dfdu_tree::{Ix, NodeFlags, NodeRef, Tree};

/// Multiply-rotate hashing for the table's integer keys (inode numbers, disk addresses): the
/// default `SipHash` is a noticeable part of the applier's time per measured file.
#[derive(Default, Clone, Copy)]
struct IntHasher(u64);

impl Hasher for IntHasher {
    fn write(&mut self, bytes: &[u8]) {
        for c in bytes.chunks(8) {
            let mut b = [0u8; 8];
            b[..c.len()].copy_from_slice(c);
            self.write_u64(u64::from_le_bytes(b));
        }
    }
    fn write_u64(&mut self, n: u64) {
        self.0 = (self.0.rotate_left(5) ^ n).wrapping_mul(0xf135_7aea_2e62_a9c5);
    }
    fn write_u32(&mut self, n: u32) {
        self.write_u64(n.into());
    }
    fn write_usize(&mut self, n: usize) {
        self.write_u64(n as u64);
    }
    fn finish(&self) -> u64 {
        self.0.rotate_left(26)
    }
}

type IntMap<K, V> = HashMap<K, V, BuildHasherDefault<IntHasher>>;

/// `(dev, ino)` of a measured file. Hard links share one entry, counted at the charged link.
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
    /// Shared extents identified by their start, `(disk address, size on disk)`: every shared
    /// extent in exact mode. Counted once per address, at the largest size given.
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
    counted_at: NodeRef,
    excl: u64,
    compressed: bool,
    ranges: Vec<(u64, u64)>,
    points: Vec<(u64, u64)>,
}

#[derive(Debug, Default)]
pub struct ExtentTable {
    fs: HashMap<[u8; 16], Fs>,
    /// The last filesystem looked up in `fs`.
    last_fs: Option<([u8; 16], Fs)>,
    segs: BTreeMap<(Fs, u64), Seg>,
    points: IntMap<(Fs, u64), Point>,
    /// Inodes with shared data, and hard links (whose measurement moves between links). Other
    /// files hold all their data alone: their node has it, and nothing needs re-homing.
    inodes: IntMap<Inode, InodeRec>,
}

impl ExtentTable {
    /// The node an inode's measurement is counted at.
    pub fn counted_at(&self, inode: Inode) -> Option<NodeRef> {
        self.inodes.get(&inode).map(|r| r.counted_at)
    }

    /// Records the measurement of the file at `node`. A measurement for an inode already
    /// counted at another live node is ignored; one counted at a dead node is replaced.
    pub fn measure(&mut self, tree: &mut Tree, inode: Inode, node: NodeRef, m: Measurement) {
        if let Some(r) = self.inodes.get(&inode) {
            if tree.resolve_visible(r.counted_at).is_some() {
                return;
            }
            self.remove(tree, inode);
        }
        let Some(ix) = tree.resolve_visible(node) else {
            return;
        };
        tree.set_measured(ix, m.excl, m.compressed);
        if m.ranges.is_empty() && m.points.is_empty() && !tree.flags(ix).has(NodeFlags::HARDLINK) {
            return;
        }
        let fs = match self.last_fs {
            Some((id, fs)) if id == m.fs => fs,
            _ => {
                #[expect(
                    clippy::cast_possible_truncation,
                    reason = "a table holds far fewer filesystems than u32::MAX"
                )]
                let next = self.fs.len() as Fs;
                let fs = *self.fs.entry(m.fs).or_insert(next);
                self.last_fs = Some((m.fs, fs));
                fs
            }
        };
        let ranges = merge(m.ranges);
        let mut points = m.points;
        points.sort_unstable();
        points.dedup_by_key(|p| p.0);
        // Re-homing needs the node the inode is counted at, not its ranges: those are stored after.
        self.inodes.insert(
            inode,
            InodeRec {
                fs,
                counted_at: node,
                excl: m.excl,
                compressed: m.compressed,
                ranges: Vec::new(),
                points: Vec::new(),
            },
        );
        for &(a, b) in &ranges {
            self.add_range(tree, fs, a, b, inode, ix);
        }
        for &(p, len) in &points {
            let pt = self.points.entry((fs, p)).or_insert(Point {
                refs: vec![],
                home: None,
                charged: 0,
                out: false,
            });
            pt.refs.push((inode, len));
            rehome_point(tree, &self.inodes, pt, Some(ix));
        }
        if let Some(rec) = self.inodes.get_mut(&inode) {
            rec.ranges = ranges;
            rec.points = points;
        }
    }

    fn add_range(&mut self, tree: &mut Tree, fs: Fs, a: u64, b: u64, inode: Inode, at: Ix) {
        self.split(fs, a);
        self.split(fs, b);
        let starts = self.starts(fs, a, b);
        let mut cur = a;
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
            }
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
        }
        // The gaps filled, the segments in `[a, b)` cover it.
        for (&(_, s), seg) in self.segs.range_mut((fs, a)..(fs, b)) {
            if !seg.refs.contains(&inode) {
                seg.refs.push(inode);
            }
            rehome_seg(tree, &self.inodes, s, seg, Some(at));
        }
    }

    /// Starts of the segments that begin in `[a, b)`.
    fn starts(&self, fs: Fs, a: u64, b: u64) -> Vec<u64> {
        self.segs
            .range((fs, a)..(fs, b))
            .map(|(&(_, s), _)| s)
            .collect()
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
        let mut emptied = Vec::new();
        for &(a, b) in &rec.ranges {
            for (&(_, s), seg) in self.segs.range_mut((fs, a)..(fs, b)) {
                seg.refs.retain(|&r| r != inode);
                rehome_seg(tree, &self.inodes, s, seg, None);
                if seg.refs.is_empty() {
                    emptied.push(s);
                }
            }
        }
        for s in emptied {
            self.segs.remove(&(fs, s));
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
        if rec.counted_at == node {
            return;
        }
        rec.counted_at = node;
        let (fs, excl, compressed) = (rec.fs, rec.excl, rec.compressed);
        let (ranges, points) = (rec.ranges.clone(), rec.points.clone());
        if let Some(ix) = tree.resolve(node) {
            tree.set_measured(ix, excl, compressed);
        }
        for (a, b) in ranges {
            for (&(_, s), seg) in self.segs.range_mut((fs, a)..(fs, b)) {
                rehome_seg(tree, &self.inodes, s, seg, None);
            }
        }
        for (p, _) in points {
            if let Some(pt) = self.points.get_mut(&(fs, p)) {
                rehome_point(tree, &self.inodes, pt, None);
            }
        }
    }

    // resume-rehome
    /// Re-homes every shared extent whose home is no longer visible. A subtree moved whole
    /// (a resume's kept directory, grafted into its rescan) keeps its references, so no freed
    /// reference tells that their home went with the tree the rescan replaced.
    pub fn rehome_detached(&mut self, tree: &mut Tree) {
        for (&(_, s), seg) in &mut self.segs {
            if seg.home.is_some_and(|h| tree.resolve_visible(h).is_none()) {
                rehome_seg(tree, &self.inodes, s, seg, None);
            }
        }
        for pt in self.points.values_mut() {
            if pt.home.is_some_and(|h| tree.resolve_visible(h).is_none()) {
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
            tree.resolve(r).map_or_else(
                || "freed".into(),
                |ix| String::from_utf8_lossy(&tree.path(ix)).into_owned(),
            )
        };
        let mut out = format!(
            "{inode:?} at {} excl {} ranges {:?} points {:?}",
            path(rec.counted_at),
            rec.excl,
            rec.ranges,
            rec.points
        );
        for &(a, b) in &rec.ranges {
            for (&(_, st), seg) in self.segs.range((rec.fs, a)..(rec.fs, b)) {
                let _ = write!(
                    out,
                    "\n  seg {st}..{} refs {:?} home {:?} out {}",
                    seg.end,
                    seg.refs,
                    seg.home.map(path),
                    seg.out
                );
            }
        }
        out
    }

    /// Checks that every measured inode is counted at a visible node.
    pub fn check_settled(&self, tree: &Tree) -> Result<(), String> {
        for (k, r) in &self.inodes {
            if tree.resolve_visible(r.counted_at).is_none() {
                return Err(format!(
                    "inode {k:?} is counted at a node that is not visible ({})",
                    if tree.resolve(r.counted_at).is_some() {
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
                            .is_some_and(|rec| tree.resolve_visible(rec.counted_at).is_none())
                    });
                    if !stale {
                        let path = |ix: Ix| String::from_utf8_lossy(&tree.path(ix)).into_owned();
                        let refs: Vec<String> = seg
                            .refs
                            .iter()
                            .map(|r| match self.inodes.get(r) {
                                Some(rec) => match tree.resolve(rec.counted_at) {
                                    Some(ix) => format!(
                                        "{r:?} at {} visible {}",
                                        path(ix),
                                        tree.resolve_visible(rec.counted_at).is_some()
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
            if tree.shared_here(ix) != bytes {
                return Err(format!(
                    "{}: home {} bytes, table {bytes}",
                    String::from_utf8_lossy(&tree.path(ix)),
                    tree.shared_here(ix)
                ));
            }
        }
        Ok(())
    }
}

/// Sorted, non-overlapping ranges (a file can reference the same data twice).
fn merge(mut r: Vec<(u64, u64)>) -> Vec<(u64, u64)> {
    r.retain(|&(a, b)| a < b);
    r.sort_unstable();
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
    inodes: &IntMap<Inode, InodeRec>,
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
    inodes: &IntMap<Inode, InodeRec>,
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
    inodes: &IntMap<Inode, InodeRec>,
    cur: Option<NodeRef>,
    refs: impl Iterator<Item = Inode>,
    added: Option<Ix>,
) -> Option<NodeRef> {
    let ix = match (added, cur.and_then(|h| tree.resolve_visible(h))) {
        (Some(a), Some(h)) => lca2(tree, h, a),
        (Some(a), None) if cur.is_none() => Some(a),
        _ => lca(tree, inodes, refs),
    };
    ix.map(|i| tree.node_ref(i))
}

/// Moves `old_len` bytes out of the old home and `new_len` into the new one, with whether
/// they are shared only outside the scan.
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
        tree.sub_shared(o, old_len, *out);
    }
    if let Some(n) = new.and_then(|h| tree.resolve(h)) {
        tree.add_shared(n, new_len, new_out);
    }
    *home = new;
    *out = new_out && new.is_some();
}

/// Lowest common ancestor of the visible nodes `refs` are counted at.
fn lca(
    tree: &Tree,
    inodes: &IntMap<Inode, InodeRec>,
    refs: impl Iterator<Item = Inode>,
) -> Option<Ix> {
    let at: Vec<Ix> = refs
        .filter_map(|r| {
            inodes
                .get(&r)
                .and_then(|rec| tree.resolve_visible(rec.counted_at))
        })
        .collect();
    lca_of(tree, &at)
}

/// [`lca_of`] for two nodes, without allocating: walks the deeper one up to the other's depth,
/// then both together.
fn lca2(tree: &Tree, a: Ix, b: Ix) -> Option<Ix> {
    let depth = |mut n: Ix| {
        let mut d = 0usize;
        while let Some(p) = tree.parent(n) {
            d += 1;
            n = p;
        }
        d
    };
    let (mut x, mut y) = (a, b);
    let (mut dx, mut dy) = (depth(x), depth(y));
    while dx > dy {
        x = tree.parent(x)?;
        dx -= 1;
    }
    while dy > dx {
        y = tree.parent(y)?;
        dy -= 1;
    }
    while x != y {
        match (tree.parent(x), tree.parent(y)) {
            (Some(px), Some(py)) => (x, y) = (px, py),
            // Different roots: the rule for those is in `lca_of`.
            _ => return lca_of(tree, &[a, b]),
        }
    }
    Some(x)
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
            .filter_map(|c| Some((tree.path(*c.last()?), c[0])))
            .min_by(|a, b| a.0.cmp(&b.0))?
            .1;
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
mod tests;
