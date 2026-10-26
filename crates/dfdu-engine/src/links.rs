//! Hard-link table owned by the applier.

use std::collections::HashMap;

use dfdu_tree::{Ix, NodeRef, Tree};

/// Scope 0 is the visible tree; a rescan's shadow uses its job id as scope.
pub type Scope = u32;
pub const VISIBLE: Scope = 0;

/// A list that holds up to two items without allocating. Nearly every inode with hard links
/// has links in one scope, and most have two; a `Vec` for each costs about a tenth of the
/// applier's time at 10M entries, in a fragmented heap.
#[derive(Debug, Default)]
enum Small<T> {
    #[default]
    Empty,
    One([T; 1]),
    Two([T; 2]),
    Many(Vec<T>),
}

impl<T> Small<T> {
    fn push(&mut self, x: T) {
        *self = match std::mem::take(self) {
            Small::Empty => Small::One([x]),
            Small::One([a]) => Small::Two([a, x]),
            Small::Two([a, b]) => Small::Many(vec![a, b, x]),
            Small::Many(mut v) => {
                v.push(x);
                Small::Many(v)
            }
        };
    }

    fn into_vec(self) -> Vec<T> {
        match self {
            Small::Empty => Vec::new(),
            Small::One(a) => a.into(),
            Small::Two(a) => a.into(),
            Small::Many(v) => v,
        }
    }

    fn retain(&mut self, mut f: impl FnMut(&T) -> bool) {
        for x in std::mem::take(self).into_vec() {
            if f(&x) {
                self.push(x);
            }
        }
    }

    fn swap_remove(&mut self, i: usize) -> T {
        let mut v = std::mem::take(self).into_vec();
        let x = v.swap_remove(i);
        for y in v {
            self.push(y);
        }
        x
    }

    fn extend(&mut self, other: Small<T>) {
        for x in other.into_vec() {
            self.push(x);
        }
    }
}

impl<T> std::ops::Deref for Small<T> {
    type Target = [T];
    fn deref(&self) -> &[T] {
        match self {
            Small::Empty => &[],
            Small::One(a) => a,
            Small::Two(a) => a,
            Small::Many(v) => v,
        }
    }
}

impl<T> std::ops::DerefMut for Small<T> {
    fn deref_mut(&mut self) -> &mut [T] {
        match self {
            Small::Empty => &mut [],
            Small::One(a) => a,
            Small::Two(a) => a,
            Small::Many(v) => v,
        }
    }
}

#[derive(Debug, Default)]
struct ScopeEntry {
    scope: Scope,
    links: Small<NodeRef>,
    charged: Option<NodeRef>,
}

/// The links of one inode, by scope.
#[derive(Debug, Default)]
struct Links {
    scopes: Small<ScopeEntry>,
    /// Freed links still in the lists. A freed link stays until the lists are pruned: finding
    /// it would cost a search of its list, and freeing a subtree removes many links of the same
    /// inodes.
    stale: usize,
    /// A prune was requested and has not run yet.
    prune_due: bool,
}

#[derive(Debug, Default)]
pub struct LinkTable {
    map: HashMap<(u64, u64), Links>,
    /// Ancestor chains for [`LinkTable::reelect`], kept to save two allocations per inode.
    chains: (Vec<Ix>, Vec<Ix>),
}

impl LinkTable {
    fn entry(&mut self, key: (u64, u64), scope: Scope) -> &mut ScopeEntry {
        let v = &mut self.map.entry(key).or_default().scopes;
        match v.iter().position(|e| e.scope == scope) {
            Some(i) => &mut v[i],
            None => {
                v.push(ScopeEntry {
                    scope,
                    ..Default::default()
                });
                v.last_mut().expect("just pushed")
            }
        }
    }

    /// Adds a link a scan found. It is charged if no other link of the inode in `scope` is;
    /// `make` creates its node, told whether it is charged. One lookup of the inode.
    pub fn add_scanned(
        &mut self,
        key: (u64, u64),
        scope: Scope,
        make: impl FnOnce(bool) -> NodeRef,
    ) {
        let e = self.entry(key, scope);
        let charged = e.charged.is_none();
        let node = make(charged);
        e.links.push(node);
        if charged {
            e.charged = Some(node);
        }
    }

    /// Records that a link was freed. Returns the scope to re-elect in if it was the charged
    /// one, and whether the inode's lists should now be pruned ([`LinkTable::prune`]): once half
    /// of their entries are freed, so removing all links of an inode costs time linear in their
    /// number. Every freed link must be removed exactly once.
    pub fn remove(&mut self, key: (u64, u64), node: NodeRef) -> (Option<Scope>, bool) {
        let Some(v) = self.map.get_mut(&key) else {
            return (None, false);
        };
        let mut reelect = None;
        for e in v.scopes.iter_mut() {
            if e.charged == Some(node) {
                e.charged = None;
                reelect = Some(e.scope);
            }
        }
        v.stale += 1;
        let len: usize = v.scopes.iter().map(|e| e.links.len()).sum();
        let prune = !v.prune_due && 2 * v.stale >= len;
        v.prune_due |= prune;
        (reelect, prune)
    }

    /// Drops the freed links of an inode, and the inode once it has none. Returns the number of
    /// links looked at.
    pub fn prune(&mut self, tree: &Tree, key: (u64, u64)) -> usize {
        let Some(v) = self.map.get_mut(&key) else {
            return 0;
        };
        let mut n = 0;
        for e in v.scopes.iter_mut() {
            n += e.links.len();
            e.links.retain(|&l| !tree.is_freed(l));
        }
        v.scopes.retain(|e| !e.links.is_empty());
        v.stale = 0;
        v.prune_due = false;
        if v.scopes.is_empty() {
            self.map.remove(&key);
        }
        n
    }

    /// The charged link of an inode in `scope`.
    pub fn charged(&self, key: (u64, u64), scope: Scope) -> Option<NodeRef> {
        self.map
            .get(&key)?
            .scopes
            .iter()
            .find(|e| e.scope == scope)?
            .charged
    }

    /// Whether any link of the inode is known, in any scope.
    pub fn has(&self, key: (u64, u64)) -> bool {
        self.map.contains_key(&key)
    }

    /// Keys with more than one link in `scope` (candidates for the final lowest-path pass).
    pub fn shared_keys(&self, scope: Scope) -> Vec<(u64, u64)> {
        self.map
            .iter()
            .filter(|(_, v)| {
                v.scopes
                    .iter()
                    .any(|e| e.scope == scope && e.links.len() > 1)
            })
            .map(|(k, _)| *k)
            .collect()
    }

    pub fn keys_in(&self, scope: Scope) -> Vec<(u64, u64)> {
        self.map
            .iter()
            .filter(|(_, v)| v.scopes.iter().any(|e| e.scope == scope))
            .map(|(k, _)| *k)
            .collect()
    }

    /// Moves `from`'s links of one inode into `to`. If both had a charged link, the moved one is
    /// uncharged and the flag is `true` so the caller re-elects. Also returns the number of links
    /// moved.
    pub fn merge_key(
        &mut self,
        tree: &mut Tree,
        key: (u64, u64),
        from: Scope,
        to: Scope,
    ) -> (bool, usize) {
        let Some(v) = self.map.get_mut(&key) else {
            return (false, 0);
        };
        let v = &mut v.scopes;
        let Some(i) = v.iter().position(|e| e.scope == from) else {
            return (false, 0);
        };
        let moved = v.swap_remove(i);
        let target = match v.iter().position(|e| e.scope == to) {
            Some(j) => &mut v[j],
            None => {
                v.push(ScopeEntry {
                    scope: to,
                    ..Default::default()
                });
                v.last_mut().expect("just pushed")
            }
        };
        let n = moved.links.len();
        target.links.extend(moved.links);
        let reelect = match (target.charged, moved.charged) {
            (Some(_), Some(m)) => {
                if let Some(ix) = tree.resolve(m) {
                    tree.set_charged(ix, false);
                }
                true
            }
            (None, Some(m)) => {
                target.charged = Some(m);
                false
            }
            _ => false,
        };
        (reelect, n)
    }

    /// Charges the live link with the lowest path in `scope`; links that do not resolve
    /// are skipped (freed ones are dropped by [`LinkTable::prune`]). Returns whether the charged
    /// link changed, and the number of links looked at.
    pub fn reelect(&mut self, tree: &mut Tree, key: (u64, u64), scope: Scope) -> (bool, usize) {
        let Some(v) = self.map.get_mut(&key) else {
            return (false, 0);
        };
        let Some(e) = v.scopes.iter_mut().find(|e| e.scope == scope) else {
            return (false, 0);
        };
        let n = e.links.len();
        // The first link with the lowest path, comparing ancestor chains, not built paths.
        let (best_chain, chain) = &mut self.chains;
        let (mut best, mut old) = (None, None);
        for &l in e.links.iter() {
            let Some(ix) = tree.resolve_ancestors(l, chain) else {
                continue;
            };
            if e.charged == Some(l) {
                old = Some(ix);
            }
            if best.is_none() || tree.cmp_paths(chain, best_chain).is_lt() {
                best = Some(l);
                std::mem::swap(best_chain, chain);
            }
        }
        if e.charged == best {
            return (false, n);
        }
        // The charged link is one of the links, found above if it resolves.
        if let Some(old) = old.or_else(|| e.charged.and_then(|c| tree.resolve(c))) {
            tree.set_charged(old, false);
        }
        if best.is_some() {
            let ix = *best_chain.last().expect("resolved");
            tree.set_charged(ix, true);
        }
        e.charged = best;
        (true, n)
    }
}
