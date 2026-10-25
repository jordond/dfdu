//! Row order of the current directory, kept stable while sizes stream in
//! (`docs/research/tui-design.md` §2.5).
//!
//! The order is rebuilt only when asked: an exact sort on entering a directory, on a sort key
//! or on `S`, and an automatic sort after the user has been idle. Between sorts, rows keep
//! their positions, entries that disappeared are dropped, and entries that appeared are
//! collected as "new" and shown after the sorted rows.

use std::cmp::Ordering;
use std::collections::{HashMap, HashSet};

use dut_tree::NodeRef;

use crate::snapshot::EntryData;

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum SortKey {
    Size,
    Name,
    Items,
    Mtime,
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct SortSpec {
    pub key: SortKey,
    pub desc: bool,
}

impl Default for SortSpec {
    fn default() -> Self {
        SortSpec {
            key: SortKey::Size,
            desc: true,
        }
    }
}

impl SortSpec {
    /// The default direction for a key: largest first for sizes and counts, A-Z for names,
    /// newest first for times.
    pub fn for_key(key: SortKey) -> Self {
        SortSpec {
            key,
            desc: key != SortKey::Name,
        }
    }
}

/// Sizes within about 2% of each other get the same bucket, so automatic re-sorts do not swap
/// two nearly equal siblings back and forth.
pub fn bucket(v: u64) -> i64 {
    if v == 0 {
        -1
    } else {
        ((v as f64).ln() / 1.02f64.ln()).floor() as i64
    }
}

fn name_cmp(a: &[u8], b: &[u8]) -> Ordering {
    let la = a.iter().map(|c| c.to_ascii_lowercase());
    let lb = b.iter().map(|c| c.to_ascii_lowercase());
    la.cmp(lb).then_with(|| a.cmp(b))
}

#[derive(Debug, Default)]
pub struct Order {
    rows: Vec<NodeRef>,
    new: Vec<NodeRef>,
}

impl Order {
    /// Sorted rows, in display order.
    pub fn rows(&self) -> &[NodeRef] {
        &self.rows
    }

    /// Entries that appeared since the last sort, in arrival order.
    pub fn new_entries(&self) -> &[NodeRef] {
        &self.new
    }

    /// Exact sort of every entry.
    pub fn sort_exact(&mut self, entries: &[EntryData], spec: SortSpec) {
        self.sort(entries, spec, false);
    }

    /// Automatic sort: sizes and counts compared by bucket, previous position as tie-break.
    pub fn sort_auto(&mut self, entries: &[EntryData], spec: SortSpec) {
        self.sort(entries, spec, true);
    }

    fn sort(&mut self, entries: &[EntryData], spec: SortSpec, quantise: bool) {
        let prev: HashMap<NodeRef, usize> = self
            .rows
            .iter()
            .chain(self.new.iter())
            .enumerate()
            .map(|(i, &r)| (r, i))
            .collect();
        let mut sorted: Vec<&EntryData> = entries.iter().collect();
        let value = |e: &EntryData| -> i64 {
            let v = match spec.key {
                SortKey::Size => e.size,
                SortKey::Items => e.items,
                _ => 0,
            };
            if quantise {
                bucket(v)
            } else {
                v.min(i64::MAX as u64) as i64
            }
        };
        sorted.sort_by(|a, b| {
            let primary = match spec.key {
                SortKey::Size | SortKey::Items => value(a).cmp(&value(b)),
                SortKey::Mtime => a.mtime.cmp(&b.mtime),
                SortKey::Name => name_cmp(&a.name, &b.name),
            };
            let primary = if spec.desc {
                primary.reverse()
            } else {
                primary
            };
            let keep = || {
                if quantise {
                    let pa = prev.get(&a.node).copied().unwrap_or(usize::MAX);
                    let pb = prev.get(&b.node).copied().unwrap_or(usize::MAX);
                    pa.cmp(&pb)
                } else {
                    Ordering::Equal
                }
            };
            primary
                .then_with(keep)
                .then_with(|| name_cmp(&a.name, &b.name))
        });
        self.rows = sorted.into_iter().map(|e| e.node).collect();
        self.new.clear();
    }

    /// Updates the order between sorts: rows whose entry is gone are dropped, entries not yet
    /// in the order are added to the new list. If nothing is shown yet, sorts at once, since
    /// there is no position to keep stable.
    pub fn sync(&mut self, entries: &[EntryData], spec: SortSpec) {
        if self.rows.is_empty() && self.new.is_empty() {
            self.sort_exact(entries, spec);
            return;
        }
        let present: HashSet<NodeRef> = entries.iter().map(|e| e.node).collect();
        self.rows.retain(|r| present.contains(r));
        self.new.retain(|r| present.contains(r));
        if self.rows.is_empty() && self.new.is_empty() {
            self.sort_exact(entries, spec);
            return;
        }
        let known: HashSet<NodeRef> = self.rows.iter().chain(self.new.iter()).copied().collect();
        for e in entries {
            if !known.contains(&e.node) {
                self.new.push(e.node);
            }
        }
    }

    /// Replaces a node that was swapped by a rescan with its successor, keeping its position.
    pub fn replace(&mut self, old: NodeRef, new: NodeRef) {
        for r in self.rows.iter_mut().chain(self.new.iter_mut()) {
            if *r == old {
                *r = new;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::snapshot::test_entry;

    fn names(o: &Order, entries: &[EntryData]) -> Vec<String> {
        o.rows()
            .iter()
            .map(|r| {
                let e = entries.iter().find(|e| e.node == *r).expect("entry");
                String::from_utf8_lossy(&e.name).into_owned()
            })
            .collect()
    }

    #[test]
    fn exact_sort_by_size_and_name() {
        let es = vec![
            test_entry(0, "b", 10),
            test_entry(1, "a", 30),
            test_entry(2, "c", 20),
        ];
        let mut o = Order::default();
        o.sort_exact(&es, SortSpec::default());
        assert_eq!(names(&o, &es), ["a", "c", "b"]);
        o.sort_exact(&es, SortSpec::for_key(SortKey::Name));
        assert_eq!(names(&o, &es), ["a", "b", "c"]);
    }

    #[test]
    fn rows_keep_positions_between_sorts_and_new_entries_wait() {
        let mut es = vec![test_entry(0, "a", 100), test_entry(1, "b", 50)];
        let mut o = Order::default();
        o.sync(&es, SortSpec::default());
        assert_eq!(names(&o, &es), ["a", "b"]);
        // "b" grows past "a" and a new entry arrives: nothing moves until a sort.
        es[1].size = 500;
        es.push(test_entry(2, "c", 1000));
        o.sync(&es, SortSpec::default());
        assert_eq!(names(&o, &es), ["a", "b"]);
        assert_eq!(o.new_entries().len(), 1);
        o.sort_auto(&es, SortSpec::default());
        assert_eq!(names(&o, &es), ["c", "b", "a"]);
        assert!(o.new_entries().is_empty());
    }

    #[test]
    fn auto_sort_does_not_swap_nearly_equal_siblings() {
        let mut es = vec![test_entry(0, "x", 1_000_000), test_entry(1, "y", 1_001_000)];
        let mut o = Order::default();
        o.sort_exact(&es, SortSpec::default());
        assert_eq!(names(&o, &es), ["y", "x"]);
        // "x" overtakes "y" by less than 2%: an automatic sort keeps the previous order...
        es[0].size = 1_003_000;
        assert_eq!(bucket(es[0].size), bucket(es[1].size));
        o.sort_auto(&es, SortSpec::default());
        assert_eq!(names(&o, &es), ["y", "x"]);
        // ...an explicit sort does not.
        o.sort_exact(&es, SortSpec::default());
        assert_eq!(names(&o, &es), ["x", "y"]);
    }

    #[test]
    fn vanished_rows_are_dropped() {
        let es = vec![
            test_entry(0, "a", 3),
            test_entry(1, "b", 2),
            test_entry(2, "c", 1),
        ];
        let mut o = Order::default();
        o.sync(&es, SortSpec::default());
        let fewer = vec![es[0].clone(), es[2].clone()];
        o.sync(&fewer, SortSpec::default());
        assert_eq!(names(&o, &fewer), ["a", "c"]);
    }
}
