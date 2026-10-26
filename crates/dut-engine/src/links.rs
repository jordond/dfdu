//! Hard-link table owned by the applier (`docs/data-pipeline.md` §5.5).

use std::collections::HashMap;

use dut_tree::{NodeRef, Tree};

/// Scope 0 is the visible tree; a rescan's shadow uses its job id as scope.
pub type Scope = u32;
pub const VISIBLE: Scope = 0;

#[derive(Debug, Default)]
struct ScopeEntry {
    scope: Scope,
    links: Vec<NodeRef>,
    charged: Option<NodeRef>,
}

/// The links of one inode, by scope.
#[derive(Debug, Default)]
struct Links {
    scopes: Vec<ScopeEntry>,
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

    /// Whether a new link in `scope` should be charged (no other link of the inode is).
    pub fn should_charge(&self, key: (u64, u64), scope: Scope) -> bool {
        self.map
            .get(&key)
            .and_then(|v| v.scopes.iter().find(|e| e.scope == scope))
            .is_none_or(|e| e.charged.is_none())
    }

    pub fn add(&mut self, key: (u64, u64), scope: Scope, node: NodeRef, charged: bool) {
        let e = self.entry(key, scope);
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

    /// Charges the live link with the lowest path in `scope` (§5.5); links that do not resolve
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
        let (mut best, mut best_chain, mut chain) = (None, Vec::new(), Vec::new());
        for &l in &e.links {
            let Some(ix) = tree.resolve(l) else { continue };
            tree.ancestors(ix, &mut chain);
            if best.is_none() || tree.cmp_paths(&chain, &best_chain).is_lt() {
                best = Some(l);
                std::mem::swap(&mut best_chain, &mut chain);
            }
        }
        if e.charged == best {
            return (false, n);
        }
        if let Some(old) = e.charged.and_then(|c| tree.resolve(c)) {
            tree.set_charged(old, false);
        }
        if let Some(b) = best {
            let ix = tree.resolve(b).expect("live");
            tree.set_charged(ix, true);
        }
        e.charged = best;
        (true, n)
    }
}
