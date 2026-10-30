//! Putting the loaded directories a resume's rescan kept in the place of their placeholders.

use dfdu_rules::{Hit, RuleIx, Safety};
use dfdu_scan::Backend;
use dfdu_scan::classify::ClassOpts;
use dfdu_tree::{Class, DirState, Ix, NodeFlags, NodeRef, Tree};

use super::Kept;
use crate::applier::{Applier, Model};

/// What decides whether two classes are the same match: the rule, and the safety and
/// degradation its sibling conditions gave it (not the flags the classifier pool adds later).
fn verdict(c: &Class) -> (RuleIx, Safety, Option<u16>) {
    (c.rule, c.safety, c.degraded)
}

/// The hits its parent's listing gives directory `kx` where it is in `tree`, against `class`:
/// with its own entries, which a kept directory's unchanged mtime vouches for, and the rules
/// decided above it, what its class is decided from.
fn hits(tree: &Tree, kx: Ix, class: &ClassOpts) -> Vec<Hit> {
    crate::applier::rescan::rescan_class(tree, kx, None, class).chain
}

/// What [`Applier::graft_kept`] leaves to do once the rescan swapped in.
pub(in crate::applier) struct Grafted {
    /// Rescanned as they are.
    again: Vec<NodeRef>,
    /// Kept directories, with the hits their parent's listing gave them where they were
    /// loaded: rescanned unless the new listing gives the same ones, since a rule that looks
    /// inside is not decided by the listing, which cannot read them.
    check: Vec<(NodeRef, Vec<Hit>)>,
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
        if let Some(c) = class {
            out.extend(
                self.check
                    .into_iter()
                    .filter(|(r, old)| {
                        tree.resolve_visible(*r)
                            .is_some_and(|kx| hits(tree, kx, c) != *old)
                    })
                    .map(|(r, _)| r),
            );
        }
        out
    }
}

impl<B: Backend> Applier<B> {
    /// Puts the loaded directories `kept` keeps in the place of their placeholders in the
    /// shadow of the rescan of `target`, which swaps in next and classified against `class`.
    /// What is rescanned once it has: a placeholder whose loaded directory is gone, a kept
    /// directory found under another name or parent, or classified otherwise by the new
    /// listing or by the rules above it, since its subtree was classified where it was
    /// ([`Grafted`]).
    pub(in crate::applier) fn graft_kept(
        m: &mut Model,
        kept: &Kept,
        target: Ix,
        class: Option<&ClassOpts>,
    ) -> Grafted {
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
            let rule = |ix: Ix| t.class(ix).map(|c| verdict(&c));
            let marks = |ix: Ix| {
                let f = t.flags(ix);
                (f.has(NodeFlags::PROTECTED), f.has(NodeFlags::EXCEPTED))
            };
            // The rules decided above it, which its subtree was classified with: above the
            // placeholder, those in the shadow and then those above `target`, which the shadow
            // replaces.
            let above = |ix: Ix| {
                let mut v = Vec::new();
                let mut cur = t.parent(ix);
                while let Some(q) = cur {
                    v.extend(t.class(q).map(|c| c.rule));
                    cur = match t.parent(q) {
                        None if !t.is_visible(q) => t.parent(target),
                        up => up,
                    };
                }
                v
            };
            let moved = t.name(px) != t.name(kx)
                || parent(px) != parent(kx)
                || rule(px).is_some_and(|r| rule(kx) != Some(r))
                || marks(px) != marks(kx)
                || above(px) != above(kx);
            // Where it was loaded, before it moves.
            let loaded = class.filter(|_| !moved).map(|c| hits(t, kx, c));
            m.tree.graft(px, kx);
            let r = m.tree.node_ref(kx);
            if moved {
                out.again.push(r);
            } else if let Some(old) = loaded {
                out.check.push((r, old));
            }
        }
        out
    }
}
