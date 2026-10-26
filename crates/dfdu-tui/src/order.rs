//! Row order of the current directory, kept stable while sizes stream in.
//!
//! The order is rebuilt only when asked: an exact sort on entering a directory, on a sort key
//! or on `S`, and an automatic sort after the user has been idle. Between sorts, rows keep
//! their positions, entries that disappeared are dropped, and entries that appeared are
//! collected as "new" and shown after the sorted rows.

use std::cmp::Ordering;

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

/// The first 8 bytes of the name in ASCII lower case, zero-padded, as a big-endian number:
/// when two prefixes differ they order names as [`name_cmp`] does, since names contain no
/// zero bytes.
fn name_prefix(name: &[u8]) -> u64 {
    let mut b = [0u8; 8];
    for (d, s) in b.iter_mut().zip(name) {
        *d = s.to_ascii_lowercase();
    }
    u64::from_be_bytes(b)
}

/// Sort key of one entry, compared field by field. With the index last the order is total, so
/// the unstable sort gives the same order as a stable one.
#[derive(PartialEq, Eq, PartialOrd, Ord)]
struct Key {
    /// The sort key's value (the name prefix for names), complemented when descending.
    first: u64,
    /// Position before the sort (automatic sorts only).
    prev: u32,
    prefix: u64,
    index: u32,
}

/// Display order as indices into the entries of the current snapshot.
#[derive(Debug, Default)]
pub struct Order {
    rows: Vec<usize>,
    new: Vec<usize>,
}

impl Order {
    /// Sorted rows, in display order.
    pub fn rows(&self) -> &[usize] {
        &self.rows
    }

    /// Entries that appeared since the last sort, in arrival order.
    pub fn new_entries(&self) -> &[usize] {
        &self.new
    }

    pub fn sort_exact(
        &mut self,
        entries: &[EntryData],
        visible: impl Fn(&EntryData) -> bool,
        spec: SortSpec,
    ) {
        self.sort(entries, visible, spec, false);
    }

    /// Automatic sort: sizes and counts compared by bucket, previous position as tie-break.
    /// The order must already refer to `entries` ([`Order::remap`]).
    pub fn sort_auto(
        &mut self,
        entries: &[EntryData],
        visible: impl Fn(&EntryData) -> bool,
        spec: SortSpec,
    ) {
        self.sort(entries, visible, spec, true);
    }

    fn sort(
        &mut self,
        entries: &[EntryData],
        visible: impl Fn(&EntryData) -> bool,
        spec: SortSpec,
        quantise: bool,
    ) {
        // Previous position of each entry; entries not in the order yet come after all others.
        let mut prev = vec![u32::MAX; if quantise { entries.len() } else { 0 }];
        if quantise {
            for (pos, &i) in self.rows.iter().chain(self.new.iter()).enumerate() {
                prev[i] = pos as u32;
            }
        }
        let value = |e: &EntryData| -> i64 {
            let v = match spec.key {
                SortKey::Size => e.size,
                SortKey::Items => e.items,
                SortKey::Mtime => return e.mtime,
                SortKey::Name => return 0,
            };
            if quantise {
                bucket(v)
            } else {
                v.min(i64::MAX as u64) as i64
            }
        };
        // A directory can have a million entries: the order is computed on one integer key per
        // entry, then runs whose key cannot decide (names sharing an 8-byte prefix) are sorted
        // again with the full comparison.
        let first = |e: &EntryData| -> u64 {
            let k = match spec.key {
                SortKey::Name => name_prefix(&e.name),
                _ => value(e) as u64 ^ 1 << 63,
            };
            if spec.desc { !k } else { k }
        };
        let mut keys: Vec<Key> = entries
            .iter()
            .enumerate()
            .filter(|(_, e)| visible(e))
            .map(|(i, e)| Key {
                first: first(e),
                prev: prev.get(i).copied().unwrap_or(u32::MAX),
                prefix: name_prefix(&e.name),
                index: i as u32,
            })
            .collect();
        keys.sort_unstable();
        let name = |k: &Key| &entries[k.index as usize].name[..];
        let exact = |a: &Key, b: &Key| {
            let by_name = || {
                a.prefix
                    .cmp(&b.prefix)
                    .then_with(|| name_cmp(name(a), name(b)))
            };
            let primary = match spec.key {
                SortKey::Name => by_name(),
                _ => a.first.cmp(&b.first),
            };
            let primary = if spec.desc && spec.key == SortKey::Name {
                primary.reverse()
            } else {
                primary
            };
            primary
                .then(a.prev.cmp(&b.prev))
                .then_with(by_name)
                .then(a.index.cmp(&b.index))
        };
        let undecided = |a: &Key, b: &Key| match spec.key {
            SortKey::Name => a.first == b.first,
            _ => (a.first, a.prev, a.prefix) == (b.first, b.prev, b.prefix),
        };
        let mut i = 0;
        while i < keys.len() {
            let mut j = i + 1;
            while j < keys.len() && undecided(&keys[i], &keys[j]) {
                j += 1;
            }
            if j - i > 1 {
                keys[i..j].sort_unstable_by(exact);
            }
            i = j;
        }
        self.rows = keys.into_iter().map(|k| k.index as usize).collect();
        self.new.clear();
    }

    /// Makes the order refer to a new list of entries: `map` gives the new index of an old
    /// one, or `None` if it is gone, and gone entries are dropped.
    pub fn remap(&mut self, map: impl Fn(usize) -> Option<usize>) {
        for list in [&mut self.rows, &mut self.new] {
            *list = list.iter().filter_map(|&i| map(i)).collect();
        }
    }

    /// Updates the order between sorts: visible entries not yet in the order are added to the
    /// new list. If nothing is shown, sorts at once, since there is no position to keep stable.
    pub fn sync(
        &mut self,
        entries: &[EntryData],
        visible: impl Fn(&EntryData) -> bool,
        spec: SortSpec,
    ) {
        if self.rows.is_empty() && self.new.is_empty() {
            self.sort_exact(entries, visible, spec);
            return;
        }
        let mut known = vec![false; entries.len()];
        for &i in self.rows.iter().chain(self.new.iter()) {
            known[i] = true;
        }
        for (i, e) in entries.iter().enumerate() {
            if !known[i] && visible(e) {
                self.new.push(i);
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
            .map(|&i| String::from_utf8_lossy(&entries[i].name).into_owned())
            .collect()
    }

    fn all(_: &EntryData) -> bool {
        true
    }

    #[test]
    fn exact_sort_by_size_and_name() {
        let es = vec![
            test_entry(0, "b", 10),
            test_entry(1, "a", 30),
            test_entry(2, "c", 20),
        ];
        let mut o = Order::default();
        o.sort_exact(&es, all, SortSpec::default());
        assert_eq!(names(&o, &es), ["a", "c", "b"]);
        o.sort_exact(&es, all, SortSpec::for_key(SortKey::Name));
        assert_eq!(names(&o, &es), ["a", "b", "c"]);
    }

    #[test]
    fn rows_keep_positions_between_sorts_and_new_entries_wait() {
        let mut es = vec![test_entry(0, "a", 100), test_entry(1, "b", 50)];
        let mut o = Order::default();
        o.sync(&es, all, SortSpec::default());
        assert_eq!(names(&o, &es), ["a", "b"]);
        // "b" grows past "a" and a new entry arrives: nothing moves until a sort.
        es[1].size = 500;
        es.push(test_entry(2, "c", 1000));
        o.sync(&es, all, SortSpec::default());
        assert_eq!(names(&o, &es), ["a", "b"]);
        assert_eq!(o.new_entries().len(), 1);
        o.sort_auto(&es, all, SortSpec::default());
        assert_eq!(names(&o, &es), ["c", "b", "a"]);
        assert!(o.new_entries().is_empty());
    }

    #[test]
    fn auto_sort_does_not_swap_nearly_equal_siblings() {
        let mut es = vec![test_entry(0, "x", 1_000_000), test_entry(1, "y", 1_001_000)];
        let mut o = Order::default();
        o.sort_exact(&es, all, SortSpec::default());
        assert_eq!(names(&o, &es), ["y", "x"]);
        // "x" overtakes "y" by less than 2%: an automatic sort keeps the previous order...
        es[0].size = 1_003_000;
        assert_eq!(bucket(es[0].size), bucket(es[1].size));
        o.sort_auto(&es, all, SortSpec::default());
        assert_eq!(names(&o, &es), ["y", "x"]);
        // ...an explicit sort does not.
        o.sort_exact(&es, all, SortSpec::default());
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
        o.sync(&es, all, SortSpec::default());
        let fewer = vec![es[0].clone(), es[2].clone()];
        o.remap(|i| [Some(0), None, Some(1)][i]);
        o.sync(&fewer, all, SortSpec::default());
        assert_eq!(names(&o, &fewer), ["a", "c"]);
    }

    /// The sort on integer keys gives the order of a stable sort with the plain comparison,
    /// including names that share long prefixes or differ only in case, and equal sizes.
    #[test]
    fn integer_keys_match_the_plain_comparison() {
        let names = [
            "a",
            "A",
            "b",
            "B",
            "same-prefix-for-sixteen-a",
            "same-prefix-for-sixteen-B",
            "Same-prefix-for-sixteen-b",
            "same-prefix-for-sixteen",
            "z",
            "Zz",
            ".hidden",
            "",
        ];
        let mut seed = 7u64;
        let mut next = |n: u64| {
            seed = seed
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            (seed >> 33) % n
        };
        let mut es = Vec::new();
        for i in 0..300u32 {
            let name = format!("{}{}", names[next(names.len() as u64) as usize], next(3));
            let mut e = test_entry(i, &name, [0, 10, 1000, 1001, 5000][next(5) as usize]);
            e.items = next(4);
            e.mtime = next(3) as i64 - 1;
            es.push(e);
        }
        for key in [SortKey::Size, SortKey::Name, SortKey::Items, SortKey::Mtime] {
            for desc in [false, true] {
                let spec = SortSpec { key, desc };
                for quantise in [false, true] {
                    // A previous order for the automatic sort's tie-break.
                    let prev: Vec<usize> = (0..es.len()).rev().filter(|i| i % 3 != 0).collect();
                    let mut o = Order {
                        rows: prev.clone(),
                        new: Vec::new(),
                    };
                    o.sort(&es, all, spec, quantise);
                    let pos = |i: usize| prev.iter().position(|&p| p == i).unwrap_or(usize::MAX);
                    let value = |e: &EntryData| -> i64 {
                        let v = match key {
                            SortKey::Size => e.size,
                            SortKey::Items => e.items,
                            SortKey::Mtime => return e.mtime,
                            SortKey::Name => return 0,
                        };
                        if quantise { bucket(v) } else { v as i64 }
                    };
                    let mut want: Vec<usize> = (0..es.len()).collect();
                    want.sort_by(|&a, &b| {
                        let (x, y) = (&es[a], &es[b]);
                        let p = match key {
                            SortKey::Name => name_cmp(&x.name, &y.name),
                            SortKey::Mtime => x.mtime.cmp(&y.mtime),
                            _ => value(x).cmp(&value(y)),
                        };
                        let p = if desc { p.reverse() } else { p };
                        let keep = if quantise {
                            pos(a).cmp(&pos(b))
                        } else {
                            Ordering::Equal
                        };
                        p.then(keep).then_with(|| name_cmp(&x.name, &y.name))
                    });
                    assert_eq!(o.rows, want, "{spec:?} quantise {quantise}");
                }
            }
        }
    }
}
