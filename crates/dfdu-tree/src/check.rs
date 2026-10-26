use crate::{Ix, NodeFlags, Totals, Tree};

/// A structural invariant violation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Violation {
    /// A directory's total differs from its own contribution plus its children's totals.
    Totals {
        node: u32,
        stored: Box<Totals>,
        computed: Box<Totals>,
    },
    /// A child's parent pointer does not point back at the directory listing it.
    ParentMismatch {
        node: u32,
        listed_under: u32,
        parent: u32,
    },
    /// A reachable child is freed or marked detached.
    DeadChild { node: u32, listed_under: u32 },
    /// A root is freed or detached.
    DeadRoot { node: u32 },
}

impl Tree {
    /// Checks totals and parent links of every node reachable from the visible roots and from
    /// `extra_roots` (rescan shadows). Cost is linear in the reachable tree; used after every
    /// slice step in debug builds and simulation tests.
    pub fn check(&self, extra_roots: &[Ix]) -> Result<(), Violation> {
        let roots: Vec<Ix> = self.roots().chain(extra_roots.iter().copied()).collect();
        for &r in &roots {
            let f = self.flags(r);
            if f.has(NodeFlags::FREE) || f.has(NodeFlags::DETACHED) {
                return Err(Violation::DeadRoot { node: r.0 });
            }
            self.check_subtree(r)?;
        }
        Ok(())
    }

    fn check_subtree(&self, root: Ix) -> Result<(), Violation> {
        // Iterative post-order: compute each directory's total from its children.
        let mut stack: Vec<(Ix, bool)> = vec![(root, false)];
        let mut sums: std::collections::HashMap<u32, Totals> = Default::default();
        while let Some((ix, expanded)) = stack.pop() {
            if self.dir(ix).is_none() {
                continue;
            }
            if !expanded {
                stack.push((ix, true));
                for c in self.children(ix) {
                    if c.0 as usize >= self.slot_count()
                        || self.is_free_slot(c.0)
                        || self.flags(c).has(NodeFlags::DETACHED)
                    {
                        return Err(Violation::DeadChild {
                            node: c.0,
                            listed_under: ix.0,
                        });
                    }
                    let p = self.raw_parent(c.0);
                    if p != ix.0 {
                        return Err(Violation::ParentMismatch {
                            node: c.0,
                            listed_under: ix.0,
                            parent: p,
                        });
                    }
                    if self.dir(c).is_some() {
                        stack.push((c, false));
                    }
                }
            } else {
                let mut t = self.own(ix);
                for c in self.children(ix) {
                    match self.dir(c) {
                        Some(_) => t.add(sums.get(&c.0).expect("child computed first")),
                        None => t.add(&self.own(c)),
                    }
                }
                let stored = self.total(ix);
                if stored != t {
                    return Err(Violation::Totals {
                        node: ix.0,
                        stored: Box::new(stored),
                        computed: Box::new(t),
                    });
                }
                sums.insert(ix.0, t);
            }
        }
        Ok(())
    }
}
