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

#[derive(Debug, Default)]
pub struct LinkTable {
    map: HashMap<(u64, u64), Vec<ScopeEntry>>,
}

impl LinkTable {
    fn entry(&mut self, key: (u64, u64), scope: Scope) -> &mut ScopeEntry {
        let v = self.map.entry(key).or_default();
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
            .and_then(|v| v.iter().find(|e| e.scope == scope))
            .is_none_or(|e| e.charged.is_none())
    }

    pub fn add(&mut self, key: (u64, u64), scope: Scope, node: NodeRef, charged: bool) {
        let e = self.entry(key, scope);
        e.links.push(node);
        if charged {
            e.charged = Some(node);
        }
    }

    /// Removes a freed link. Returns the scope to re-elect in if it was the charged one.
    pub fn remove(&mut self, key: (u64, u64), node: NodeRef) -> Option<Scope> {
        let v = self.map.get_mut(&key)?;
        let mut reelect = None;
        for e in v.iter_mut() {
            if let Some(i) = e.links.iter().position(|&l| l == node) {
                e.links.swap_remove(i);
                if e.charged == Some(node) {
                    e.charged = None;
                    reelect = Some(e.scope);
                }
            }
        }
        v.retain(|e| !e.links.is_empty());
        if v.is_empty() {
            self.map.remove(&key);
        }
        reelect
    }

    /// The charged link of an inode in `scope`.
    pub fn charged(&self, key: (u64, u64), scope: Scope) -> Option<NodeRef> {
        self.map
            .get(&key)?
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
            .filter(|(_, v)| v.iter().any(|e| e.scope == scope && e.links.len() > 1))
            .map(|(k, _)| *k)
            .collect()
    }

    pub fn keys_in(&self, scope: Scope) -> Vec<(u64, u64)> {
        self.map
            .iter()
            .filter(|(_, v)| v.iter().any(|e| e.scope == scope))
            .map(|(k, _)| *k)
            .collect()
    }

    /// Moves `from`'s links of one inode into `to`. If both had a charged link, the moved one is
    /// uncharged and `true` is returned so the caller re-elects.
    pub fn merge_key(&mut self, tree: &mut Tree, key: (u64, u64), from: Scope, to: Scope) -> bool {
        let Some(v) = self.map.get_mut(&key) else {
            return false;
        };
        let Some(i) = v.iter().position(|e| e.scope == from) else {
            return false;
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
        target.links.extend(moved.links);
        match (target.charged, moved.charged) {
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
        }
    }

    /// Charges the live link with the lowest path in `scope` (§5.5). Dead links are pruned.
    /// Returns whether the charged link changed.
    pub fn reelect(&mut self, tree: &mut Tree, key: (u64, u64), scope: Scope) -> bool {
        let Some(v) = self.map.get_mut(&key) else {
            return false;
        };
        let Some(e) = v.iter_mut().find(|e| e.scope == scope) else {
            return false;
        };
        e.links.retain(|&l| tree.resolve(l).is_some());
        let best = e
            .links
            .iter()
            .map(|&l| (tree.path(tree.resolve(l).expect("live")), l))
            .min_by(|a, b| a.0.cmp(&b.0))
            .map(|(_, l)| l);
        if e.charged == best {
            return false;
        }
        if let Some(old) = e.charged.and_then(|c| tree.resolve(c)) {
            tree.set_charged(old, false);
        }
        if let Some(b) = best {
            let ix = tree.resolve(b).expect("live");
            tree.set_charged(ix, true);
        }
        e.charged = best;
        if e.links.is_empty() {
            v.retain(|e| !e.links.is_empty());
            if v.is_empty() {
                self.map.remove(&key);
            }
        }
        true
    }
}
