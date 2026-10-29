//! Rule classes of nodes, and protected subtrees.

use super::{Ix, NodeFlags, Tree};
use crate::Class;

impl Tree {
    /// Sets or clears the rule node `ix` matched.
    pub fn set_class(&mut self, ix: Ix, class: Option<Class>) {
        match class {
            Some(c) => self.classes.insert(ix.0, c),
            None => self.classes.remove(&ix.0),
        };
    }

    #[must_use]
    pub fn class(&self, ix: Ix) -> Option<Class> {
        if self.classes.is_empty() {
            return None;
        }
        self.classes.get(&ix.0).copied()
    }

    /// Every classified node, in no order.
    pub fn classes(&self) -> impl Iterator<Item = (Ix, Class)> + '_ {
        self.classes.iter().map(|(&i, &c)| (Ix(i), c))
    }

    /// Marks node `ix` as the top of a protected subtree, of an exception in one, or neither.
    /// These flags are not counted in totals, so no ancestor changes.
    pub fn set_protection(&mut self, ix: Ix, protected: bool, excepted: bool) {
        let f = &mut self.nodes[ix.0 as usize].flags;
        f.set(NodeFlags::PROTECTED, protected);
        f.set(NodeFlags::EXCEPTED, excepted);
    }

    /// Whether node `ix` is in a protected subtree: the nearest node at or above it marked
    /// [`NodeFlags::PROTECTED`] or [`NodeFlags::EXCEPTED`] is a protected one.
    #[must_use]
    pub fn is_protected(&self, ix: Ix) -> bool {
        let mut cur = Some(ix);
        while let Some(i) = cur {
            let f = self.flags(i);
            if f.has(NodeFlags::PROTECTED) {
                return true;
            }
            if f.has(NodeFlags::EXCEPTED) {
                return false;
            }
            cur = self.parent(i);
        }
        false
    }

    /// The class that applies to node `ix`, and the node it is on: [`Tree::class_below`] taken
    /// down from the root. The nearest classified node at or above `ix` wins, so a match inside
    /// another (pip's cache in the user caches) is what it is; but a match whose rule stops the
    /// descent (`stops`) covers everything below it, so the highest such node wins over any
    /// below it. Those exist when the rule above was decided only after its listing's
    /// subdirectories were queued (`dfdu_scan::classify`).
    #[must_use]
    pub fn class_in_effect(&self, ix: Ix, stops: impl Fn(&Class) -> bool) -> Option<(Ix, Class)> {
        if self.classes.is_empty() {
            return None;
        }
        let mut chain = Vec::new();
        let mut cur = Some(ix);
        while let Some(i) = cur {
            chain.push(i);
            cur = self.parent(i);
        }
        chain
            .into_iter()
            .rev()
            .fold(None, |above, i| self.class_below(above, i, &stops))
    }

    /// The class in effect at node `ix` given `above`, the class in effect at its parent: that
    /// one when its rule stops the descent (`stops`), otherwise `ix`'s own class if it has one,
    /// otherwise still `above`. The one step [`Tree::class_in_effect`] is made of, for walks
    /// that go down the tree anyway.
    #[must_use]
    pub fn class_below(
        &self,
        above: Option<(Ix, Class)>,
        ix: Ix,
        stops: impl Fn(&Class) -> bool,
    ) -> Option<(Ix, Class)> {
        match above {
            Some((_, c)) if stops(&c) => above,
            _ => self.class(ix).map(|c| (ix, c)).or(above),
        }
    }
}

#[cfg(test)]
mod tests;
