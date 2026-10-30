//! Homing again, a step at a time, the shared extents whose home is no longer visible though
//! no freed reference says so: a subtree moved whole (a resume's kept directory, grafted into
//! its rescan) keeps its references, while their home went with the tree the rescan replaced.

use dfdu_tree::Tree;

use super::{ExtentTable, Fs, rehome_point, rehome_seg};

/// A pass over the whole table: the segments in order from `segs`, then the points, by their
/// keys as the pass reached them.
#[derive(Debug)]
pub struct Rehome {
    /// The first segment not looked at, `None` once all were.
    segs: Option<(Fs, u64)>,
    /// The points not looked at, once the segments were.
    points: Option<Vec<(Fs, u64)>>,
}

impl Default for Rehome {
    fn default() -> Self {
        Rehome {
            segs: Some((0, 0)),
            points: None,
        }
    }
}

impl ExtentTable {
    /// Looks at up to `budget` shared extents of pass `r`, homing again those whose home is
    /// not visible; whether the pass has looked at all of them.
    pub fn rehome_step(&mut self, tree: &mut Tree, r: &mut Rehome, budget: usize) -> bool {
        let mut n = 0;
        if let Some(from) = r.segs {
            r.segs = None;
            for (&(fs, s), seg) in self.segs.range_mut(from..) {
                if n == budget {
                    r.segs = Some((fs, s));
                    return false;
                }
                n += 1;
                if seg.home.is_some_and(|h| tree.resolve_visible(h).is_none()) {
                    rehome_seg(tree, &self.inodes, s, seg, None);
                }
            }
        }
        let points = r
            .points
            .get_or_insert_with(|| self.points.keys().copied().collect());
        while n < budget
            && let Some(k) = points.pop()
        {
            n += 1;
            if let Some(pt) = self.points.get_mut(&k)
                && pt.home.is_some_and(|h| tree.resolve_visible(h).is_none())
            {
                rehome_point(tree, &self.inodes, pt, None);
            }
        }
        points.is_empty()
    }
}
