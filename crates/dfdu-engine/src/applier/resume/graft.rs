//! Putting the loaded directories a resume's rescan kept in the place of their placeholders:
//! the loaded directory of each placeholder is found a step at a time once the rescan has read
//! everything ([`Applier::find_kept`]), all are put in place at once as it swaps in
//! ([`Applier::graft_kept`]), and those that moved or that the new listing may classify
//! otherwise are rescanned, a step at a time, once it has ([`Grafted`]).

use std::collections::HashSet;

use dfdu_rules::{Hit, RuleIx, Safety};
use dfdu_scan::Backend;
use dfdu_scan::classify::ClassOpts;
use dfdu_tree::{Class, DirState, Ix, NodeFlags, NodeRef, Tree};

use super::Kept;
use crate::applier::work::Work;
use crate::applier::{Applier, Model};

/// Placeholders found, or kept directories judged again, per step: classifying a directory as
/// its parent's listing gives it takes a few µs.
const PER_STEP: usize = 256;

/// Kept directories rescanned per step: starting a rescan looks at every rescan running.
const RESCANS_PER_STEP: usize = 16;

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

/// The loaded directories found for a rescan's placeholders so far.
#[derive(Default)]
pub(super) struct Finding {
    /// Placeholders looked at.
    next: usize,
    /// Loaded directories found for one: each takes the place of one placeholder only.
    taken: HashSet<NodeRef>,
    found: Vec<Found>,
}

/// What a placeholder stands for.
enum Found {
    /// None of the loaded directories: it is rescanned.
    Lost(NodeRef),
    /// A loaded directory found under another name or parent, or classified otherwise by the
    /// rules above it: rescanned once in place, since its subtree was classified where it was.
    Moved(NodeRef, NodeRef),
    /// A loaded directory where it was, with the hits its parent's listing gave it there when
    /// the rescan classifies.
    Same(NodeRef, NodeRef, Option<Vec<Hit>>),
}

/// What [`Applier::graft_kept`] leaves to do once the rescan swapped in, a step at a time.
pub(in crate::applier) struct Grafted {
    /// What the rescan classified against.
    class: Option<ClassOpts>,
    /// Rescanned as they are.
    again: Vec<NodeRef>,
    /// Kept directories, with the hits their parent's listing gave them where they were
    /// loaded: rescanned unless the new listing gives the same ones, since a rule that looks
    /// inside is not decided by the listing, which cannot read them.
    check: Vec<(NodeRef, Vec<Hit>)>,
    /// Shared extents were homed in what the rescan replaced, above a kept directory.
    pub(in crate::applier) rehome: bool,
}

impl Grafted {
    /// A step's worth of the kept directories to rescan, judged in the visible tree; whether
    /// that is the last.
    fn step(&mut self, tree: &Tree) -> (Vec<NodeRef>, bool) {
        if self.again.is_empty() {
            let n = self.check.len().saturating_sub(PER_STEP);
            let check = self.check.split_off(n);
            if let Some(c) = &self.class {
                self.again.extend(
                    check
                        .into_iter()
                        .filter(|(r, old)| {
                            tree.resolve_visible(*r)
                                .is_some_and(|kx| hits(tree, kx, c) != *old)
                        })
                        .map(|(r, _)| r),
                );
            }
        }
        let n = self.again.len().saturating_sub(RESCANS_PER_STEP);
        let out = self.again.split_off(n);
        (out, self.again.is_empty() && self.check.is_empty())
    }
}

/// Whether shared extents are homed in what the rescan of `target` replaces, above the kept
/// directory of one of `pairs`: the kept subtrees keep their references, so no freed one tells
/// that such a home went.
fn homed_above(tree: &Tree, pairs: &[(Ix, Ix)], target: Ix) -> bool {
    let mut seen = HashSet::new();
    pairs.iter().any(|&(_, kx)| {
        let mut cur = tree.parent(kx);
        while let Some(c) = cur
            && seen.insert(c)
        {
            if tree.shared_here(c) > 0 {
                return true;
            }
            cur = tree.parent(c).filter(|_| c != target);
        }
        false
    })
}

impl<B: Backend> Applier<B> {
    /// Finds a step's worth of the loaded directories `kept` keeps for its placeholders, in
    /// the shadow of the rescan of `target`, which classified against `class`; whether all
    /// are found. A placeholder is rescanned when its loaded directory is gone, and a kept
    /// directory when found under another name or parent, or classified otherwise by the new
    /// listing or by the rules above it, since its subtree was classified where it was.
    pub(in crate::applier) fn find_kept(
        m: &Model,
        kept: &mut Kept,
        target: Ix,
        class: Option<&ClassOpts>,
    ) -> bool {
        let t = &m.tree;
        let f = &mut kept.finding;
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
        // Whether the rules above differ, for the last two parents: siblings come together.
        let (mut last, mut differ) = (None, false);
        let mut n = 0;
        while n < PER_STEP
            && let Some(&p) = kept.placeholders.get(f.next)
        {
            f.next += 1;
            let Some(px) = t.resolve(p) else {
                continue;
            };
            n += 1;
            let found = kept
                .nodes
                .get(&t.dev_ino(px))
                .and_then(|&(r, _)| t.resolve(r))
                .filter(|&kx| !f.taken.contains(&t.node_ref(kx)))
                .filter(|&kx| Self::is_ancestor(t, target, kx));
            let Some(kx) = found else {
                f.found.push(Found::Lost(p));
                continue;
            };
            let r = t.node_ref(kx);
            f.taken.insert(r);
            let parents = Some((t.parent(px), t.parent(kx)));
            if last != parents {
                (last, differ) = (parents, above(px) != above(kx));
            }
            let moved = t.name(px) != t.name(kx)
                || parent(px) != parent(kx)
                || rule(px).is_some_and(|r| rule(kx) != Some(r))
                || marks(px) != marks(kx)
                || differ;
            f.found.push(if moved {
                Found::Moved(p, r)
            } else {
                // Where it was loaded, before it moves.
                Found::Same(p, r, class.map(|c| hits(t, kx, c)))
            });
        }
        f.next >= kept.placeholders.len()
    }

    /// Puts the loaded directories found for `kept`'s placeholders ([`Applier::find_kept`])
    /// in their place, at once, in the shadow of the rescan of `target`, which swaps in next
    /// and classified against `class`; what is left to do once it has.
    pub(in crate::applier) fn graft_kept(
        m: &mut Model,
        kept: &mut Kept,
        target: Ix,
        class: Option<ClassOpts>,
    ) -> Grafted {
        let mut out = Grafted {
            class,
            again: Vec::new(),
            check: Vec::new(),
            rehome: false,
        };
        let (mut pairs, mut lost) = (Vec::new(), Vec::new());
        for f in std::mem::take(&mut kept.finding.found) {
            let (p, r, loaded) = match f {
                Found::Lost(p) => {
                    lost.push(p);
                    continue;
                }
                Found::Moved(p, r) => (p, r, None),
                Found::Same(p, r, loaded) => (p, r, Some(loaded)),
            };
            let (Some(px), Some(kx)) = (m.tree.resolve(p), m.tree.resolve(r)) else {
                lost.push(p);
                continue;
            };
            pairs.push((px, kx));
            match loaded {
                None => out.again.push(r),
                Some(Some(old)) => out.check.push((r, old)),
                Some(None) => {}
            }
        }
        for p in lost {
            if let Some(d) = m.tree.resolve(p).and_then(|px| m.tree.dir_mut(px)) {
                d.state = DirState::Cancelled;
                d.outcome = DirState::Cancelled;
                out.again.push(p);
            }
        }
        out.rehome = homed_above(&m.tree, &pairs, target);
        m.tree.graft(&pairs);
        out
    }

    /// Homes again the shared extents whose homes went with what a resume's rescans replaced
    /// ([`Grafted::rehome`]) once none is still to swap in, in a pass over the whole table a
    /// step at a time ([`Work::Rehome`]). Until then those extents are counted at no visible
    /// directory.
    pub(in crate::applier) fn rehome_when_swapped(&mut self) {
        let swapping = self
            .jobs
            .values()
            .any(|j| j.kept.is_some() && j.swap_pending());
        if self.resume.rehome && !swapping {
            self.resume.rehome = false;
            self.work.push_back(Work::Rehome(Box::default()));
        }
    }

    /// Rescans a step's worth of the kept directories a graft left to rescan ([`Grafted`]).
    pub(in crate::applier) fn grafted_step(&mut self, m: &mut Model, mut g: Box<Grafted>) {
        let (rescans, last) = g.step(&m.tree);
        for r in rescans {
            self.rescan(m, r, None, false);
        }
        if !last {
            self.work.push_front(Work::Grafted(g));
        }
    }
}
