//! The matches that the cleanup view, `dfdu report` and `dfdu clean` list. A match below a
//! rule that stops the descent is never made; below one that does not, such as the user
//! caches (`~/.cache`), the specific rules still apply, and their matches are listed on their
//! own. The match around them yields to them: it is listed for the rest of what it holds, its
//! size leaves them out, and cleaning it up deletes only that rest ([`Listed::rest`]). With
//! nothing else in it, it is not listed at all. So each byte is counted once in totals. Which
//! match covers an entry is [`RuleSet::class_in_effect`], as everywhere else.

use std::cmp::Reverse;
use std::collections::{HashMap, HashSet};

use dfdu_rules::RuleSet;
use dfdu_tree::{Class, DirState, HostPath, Ix, Totals, Tree};

/// One listed match.
#[derive(Clone, Debug, PartialEq)]
pub struct Listed {
    pub ix: Ix,
    pub path: HostPath,
    pub class: Class,
    /// It is a directory still being scanned: its size can grow.
    pub scanning: bool,
    /// The nearest matches inside it, listed on their own.
    pub inner: Vec<Ix>,
    /// With matches inside it, what cleaning it up deletes: the entries below it that neither
    /// are nor hold one of them. Empty when it is deleted whole.
    pub rest: Vec<Ix>,
    /// Its totals without the matches inside it.
    pub total: Totals,
}

/// Whether directory `ix` is still being scanned.
fn scanning(t: &Tree, ix: Ix) -> bool {
    t.dir(ix).is_some_and(|d| d.state == DirState::Pending)
}

/// Every visible match that is not below one whose rule stops the descent, the largest on disk
/// first (then by path).
#[must_use]
pub fn listed(t: &Tree, rules: &RuleSet) -> Vec<Listed> {
    let mut out: Vec<Listed> = Vec::new();
    let mut at: HashMap<Ix, usize> = HashMap::new();
    let mut nested: Vec<(Ix, Ix)> = Vec::new();
    for (ix, class) in t.classes().filter(|&(ix, _)| t.is_visible(ix)) {
        // What every view says covers it: itself, unless a match above stops the descent.
        if rules.class_in_effect(t, ix).is_none_or(|(at, _)| at != ix) {
            continue;
        }
        // The nearest match around it, which yields to it.
        if let Some((o, _)) = t.parent(ix).and_then(|p| rules.class_in_effect(t, p)) {
            nested.push((o, ix));
        }
        at.insert(ix, out.len());
        out.push(Listed {
            ix,
            path: t.host_path(ix),
            class,
            scanning: scanning(t, ix),
            inner: Vec::new(),
            rest: Vec::new(),
            total: t.total(ix),
        });
    }
    for (o, i) in nested {
        if let Some(&k) = at.get(&o) {
            out[k].inner.push(i);
        }
    }
    for l in out.iter_mut().filter(|l| !l.inner.is_empty()) {
        for &i in &l.inner {
            l.total = minus(l.total, &t.total(i));
        }
        l.rest = rest(t, l.ix, &l.inner);
    }
    out.retain(|l| l.inner.is_empty() || !l.rest.is_empty());
    out.sort_by(|a, b| (Reverse(a.total.alloc), &a.path).cmp(&(Reverse(b.total.alloc), &b.path)));
    out
}

/// The entries below `top` that neither are one of `inner` (nodes below it) nor hold one.
fn rest(t: &Tree, top: Ix, inner: &[Ix]) -> Vec<Ix> {
    let inner: HashSet<Ix> = inner.iter().copied().collect();
    let mut holds: HashSet<Ix> = HashSet::new();
    for &i in &inner {
        let mut cur = t.parent(i);
        while let Some(p) = cur {
            if p == top || !holds.insert(p) {
                break;
            }
            cur = t.parent(p);
        }
    }
    let mut out = Vec::new();
    let mut stack = vec![top];
    while let Some(d) = stack.pop() {
        for c in t.children(d) {
            if inner.contains(&c) || !t.is_visible(c) {
                continue;
            }
            if holds.contains(&c) {
                stack.push(c);
            } else {
                out.push(c);
            }
        }
    }
    out
}

/// `a` without `b`, which is part of it.
fn minus(mut a: Totals, b: &Totals) -> Totals {
    a.apparent = a.apparent.saturating_sub(b.apparent);
    a.alloc = a.alloc.saturating_sub(b.alloc);
    a.referenced = a.referenced.saturating_sub(b.referenced);
    a.shared_out = a.shared_out.saturating_sub(b.shared_out);
    a.items = a.items.saturating_sub(b.items);
    a.errors = a.errors.saturating_sub(b.errors);
    a.mounts = a.mounts.saturating_sub(b.mounts);
    a.ro_subvols = a.ro_subvols.saturating_sub(b.ro_subvols);
    a
}

#[cfg(test)]
mod tests;
