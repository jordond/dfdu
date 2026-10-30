//! Putting the loaded directories a resume's rescan kept in the place of their placeholders.

use dfdu_scan::Backend;
use dfdu_scan::classify::ClassOpts;
use dfdu_tree::{DirState, Ix, NodeFlags, NodeRef, Tree};

use super::Kept;
use crate::applier::{Applier, Model};

/// Whether the class `kx` was loaded with still holds where it is now in the visible tree, as
/// far as its parent's listing decides: its rule is among the hits the listing gives it. A
/// rule that looks inside `kx` is decided by its loaded entries, which the listing does not
/// read, so a hit is all it can say; one whose sibling condition no longer holds (a marker
/// file removed) gives none.
fn class_holds(tree: &Tree, kx: Ix, class: &ClassOpts) -> bool {
    let Some(rule) = tree.class(kx).map(|c| c.rule) else {
        return true;
    };
    crate::applier::rescan::rescan_class(tree, kx, None, class)
        .chain
        .iter()
        .any(|h| h.rule == rule)
}

/// What [`Applier::graft_kept`] leaves to do once the rescan swapped in.
pub(in crate::applier) struct Grafted {
    /// Rescanned as they are.
    again: Vec<NodeRef>,
    /// Kept directories classified as loaded that the new listing did not classify: rescanned
    /// unless their class still holds ([`class_holds`]).
    check: Vec<NodeRef>,
}

impl Grafted {
    /// The kept directories to rescan, judged in the visible tree against `class`, what the
    /// rescan classified against.
    pub(in crate::applier) fn rescans(
        self,
        tree: &Tree,
        class: Option<&ClassOpts>,
    ) -> Vec<NodeRef> {
        let mut out = self.again;
        out.extend(
            self.check
                .into_iter()
                .filter(|&r| match (tree.resolve_visible(r), class) {
                    (Some(kx), Some(c)) => !class_holds(tree, kx, c),
                    _ => false,
                }),
        );
        out
    }
}

impl<B: Backend> Applier<B> {
    /// Puts the loaded directories `kept` keeps in the place of their placeholders in the
    /// shadow of the rescan of `target`, which swaps in next. What is rescanned once it has: a
    /// placeholder whose loaded directory is gone, a kept directory found under another name
    /// or parent, or that the new listing classifies otherwise, since its subtree was
    /// classified where it was ([`Grafted`]).
    pub(in crate::applier) fn graft_kept(m: &mut Model, kept: &Kept, target: Ix) -> Grafted {
        let mut out = Grafted {
            again: Vec::new(),
            check: Vec::new(),
        };
        for &p in &kept.placeholders {
            let Some(px) = m.tree.resolve(p) else {
                continue;
            };
            let found = kept
                .nodes
                .get(&m.tree.dev_ino(px))
                .and_then(|&(r, _)| m.tree.resolve(r))
                .filter(|&kx| Self::is_ancestor(&m.tree, target, kx));
            let Some(kx) = found else {
                if let Some(d) = m.tree.dir_mut(px) {
                    d.state = DirState::Cancelled;
                    d.outcome = DirState::Cancelled;
                }
                out.again.push(p);
                continue;
            };
            let t = &m.tree;
            let parent = |ix: Ix| t.parent(ix).map(|q| t.dev_ino(q));
            let rule = |ix: Ix| t.class(ix).map(|c| c.rule);
            let marks = |ix: Ix| {
                let f = t.flags(ix);
                (f.has(NodeFlags::PROTECTED), f.has(NodeFlags::EXCEPTED))
            };
            let moved = t.name(px) != t.name(kx)
                || parent(px) != parent(kx)
                || rule(px).is_some_and(|r| rule(kx) != Some(r))
                || marks(px) != marks(kx);
            // Not classified by the listing, but as loaded: the listing cannot decide a rule
            // that looks inside, so whether it still holds is judged once it swapped in.
            let unsure = !moved && rule(px).is_none() && rule(kx).is_some();
            m.tree.graft(px, kx);
            if moved {
                out.again.push(m.tree.node_ref(kx));
            } else if unsure {
                out.check.push(m.tree.node_ref(kx));
            }
        }
        out
    }
}
