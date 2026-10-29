//! Row order of the current directory, kept stable while sizes stream in.
//!
//! The order is rebuilt only when asked: an exact sort on entering a directory, on a sort key
//! or on `S`, and an automatic sort after the user has been idle. Between sorts, rows keep
//! their positions, entries that disappeared are dropped, and entries that appeared are
//! collected as "new" and shown after the sorted rows.

use std::cmp::Ordering;

use crate::model::snapshot::EntryData;

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
#[expect(
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    reason = "buckets are 2% wide, far coarser than f64 precision; ln(u64::MAX) / ln(1.02) is about 2240"
)]
pub fn bucket(v: u64) -> i64 {
    if v == 0 {
        -1
    } else {
        ((v as f64).ln() / 1.02f64.ln()).floor() as i64
    }
}

fn name_cmp(a: &[u8], b: &[u8]) -> Ordering {
    let la = a.iter().map(u8::to_ascii_lowercase);
    let lb = b.iter().map(u8::to_ascii_lowercase);
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
    base: Base,
}

/// An exact sort of every entry a filter can show, while it still holds for the entries: a
/// changed filter picks its rows from it ([`Order::refilter`]). An exact sort is a total order
/// that does not depend on which entries take part, so the entries it keeps are in the order an
/// exact sort of only those would give.
#[derive(Debug, Default)]
enum Base {
    /// None, or the entries or the order changed since.
    #[default]
    None,
    /// The rows are that sort, by this spec.
    Rows(SortSpec),
    /// That sort, by this spec, kept apart from the rows.
    Kept(SortSpec, Vec<usize>),
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

    /// Sorts the entries for which `visible(index, entry)` holds.
    pub fn sort_exact(
        &mut self,
        entries: &[EntryData],
        visible: impl Fn(usize, &EntryData) -> bool,
        spec: SortSpec,
    ) {
        self.sort(entries, visible, spec, false);
    }

    /// Automatic sort: sizes and counts compared by bucket, previous position as tie-break.
    /// The order must already refer to `entries` ([`Order::remap`]).
    pub fn sort_auto(
        &mut self,
        entries: &[EntryData],
        visible: impl Fn(usize, &EntryData) -> bool,
        spec: SortSpec,
    ) {
        self.sort(entries, visible, spec, true);
    }

    /// [`Order::sort_exact`] when `visible` hides only entries that no filter shows: the rows are
    /// then also the order a filter picks its rows from ([`Order::refilter`]).
    pub fn sort_exact_unfiltered(
        &mut self,
        entries: &[EntryData],
        visible: impl Fn(usize, &EntryData) -> bool,
        spec: SortSpec,
    ) {
        self.sort_exact(entries, visible, spec);
        self.base = Base::Rows(spec);
    }

    /// Keeps an exact sort by `spec` of the entries `shown` passes (every entry a filter can
    /// show) for [`Order::refilter`], unless one is kept already. The rows stay as they are.
    pub fn keep_exact(
        &mut self,
        entries: &[EntryData],
        shown: impl Fn(usize, &EntryData) -> bool,
        spec: SortSpec,
    ) {
        match &self.base {
            Base::Rows(s) | Base::Kept(s, _) if *s == spec => {}
            _ => self.base = Base::Kept(spec, sorted(entries, shown, spec, &[])),
        }
    }

    /// Rows for a changed filter without sorting again: the entries `visible` passes, in the
    /// order of the exact sort by `spec` kept for it, which they all must be part of. False,
    /// and nothing changes, when no such sort holds for the entries.
    pub fn refilter(
        &mut self,
        entries: &[EntryData],
        visible: impl Fn(usize, &EntryData) -> bool,
        spec: SortSpec,
    ) -> bool {
        let base = match std::mem::take(&mut self.base) {
            Base::Rows(s) if s == spec => std::mem::take(&mut self.rows),
            Base::Kept(s, base) if s == spec => base,
            other => {
                self.base = other;
                return false;
            }
        };
        // Tested in entry order, so the pass over the kept order does not touch the entries.
        let keep: Vec<bool> = entries
            .iter()
            .enumerate()
            .map(|(i, e)| visible(i, e))
            .collect();
        self.rows.clear();
        self.rows.extend(base.iter().copied().filter(|&i| keep[i]));
        self.new.clear();
        self.base = Base::Kept(spec, base);
        true
    }

    #[expect(
        clippy::cast_possible_truncation,
        reason = "positions are rows of one directory, fewer than 2^32"
    )]
    fn sort(
        &mut self,
        entries: &[EntryData],
        visible: impl Fn(usize, &EntryData) -> bool,
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
        self.rows = sorted(entries, visible, spec, &prev);
        self.new.clear();
        self.base = Base::None;
    }

    /// Makes the order refer to a new list of entries: `map` gives the new index of an old
    /// one, or `None` if it is gone, and gone entries are dropped.
    pub fn remap(&mut self, map: impl Fn(usize) -> Option<usize>) {
        for list in [&mut self.rows, &mut self.new] {
            *list = list.iter().filter_map(|&i| map(i)).collect();
        }
        self.base = Base::None;
    }

    /// Updates the order between sorts: visible entries not yet in the order are added to the
    /// new list. If nothing is shown, sorts at once, since there is no position to keep stable.
    pub fn sync(
        &mut self,
        entries: &[EntryData],
        visible: impl Fn(usize, &EntryData) -> bool,
        spec: SortSpec,
    ) {
        if self.rows.is_empty() && self.new.is_empty() {
            self.sort_exact(entries, visible, spec);
            return;
        }
        // Sizes may have changed too: an exact sort kept from before no longer holds.
        self.base = Base::None;
        let mut known = vec![false; entries.len()];
        for &i in self.rows.iter().chain(self.new.iter()) {
            known[i] = true;
        }
        for (i, e) in entries.iter().enumerate() {
            if !known[i] && visible(i, e) {
                self.new.push(i);
            }
        }
    }
}

/// The entries `visible` passes, sorted by `spec`. `prev[i]`: the position of entry `i` before
/// an automatic sort, used as a tie-break (missing: after all others); empty for an exact sort.
#[expect(
    clippy::cast_possible_truncation,
    reason = "keys hold entry indices in u32 to stay small; a directory has fewer than 2^32 entries"
)]
fn sorted(
    entries: &[EntryData],
    visible: impl Fn(usize, &EntryData) -> bool,
    spec: SortSpec,
    prev: &[u32],
) -> Vec<usize> {
    let quantise = !prev.is_empty();
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
            i64::try_from(v).unwrap_or(i64::MAX)
        }
    };
    // A directory can have a million entries: the order is computed on one integer key per
    // entry, then runs whose key cannot decide (names sharing an 8-byte prefix) are sorted
    // again with the full comparison.
    let first = |e: &EntryData| -> u64 {
        let k = match spec.key {
            SortKey::Name => name_prefix(&e.name),
            _ => value(e).cast_unsigned() ^ 1 << 63,
        };
        if spec.desc { !k } else { k }
    };
    let mut keys: Vec<Key> = entries
        .iter()
        .enumerate()
        .filter(|&(i, e)| visible(i, e))
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
    keys.into_iter().map(|k| k.index as usize).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::snapshot::test_entry;

    fn names(o: &Order, entries: &[EntryData]) -> Vec<String> {
        o.rows()
            .iter()
            .map(|&i| String::from_utf8_lossy(&entries[i].name).into_owned())
            .collect()
    }

    fn all(_: usize, _: &EntryData) -> bool {
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
        assert_eq!(o.new_entries(), [] as [usize; 0]);
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

    /// Entries with repeated sizes, counts, times and names that differ only in case or after a
    /// long shared prefix, so that every tie-break of the exact sort takes part.
    fn ties(n: u32) -> Vec<EntryData> {
        let names = [
            "a",
            "A",
            "same-prefix-for-sixteen-a",
            "Same-prefix-for-sixteen-a",
            ".h",
        ];
        (0..n)
            .map(|i| {
                let name = format!("{}{}", names[(i * 7 % 5) as usize], i % 3);
                let mut e = test_entry(i, &name, [0, 10, 1000, 1001][(i * 3 % 4) as usize]);
                e.items = u64::from(i % 4);
                e.mtime = i64::from(i % 3) - 1;
                e
            })
            .collect()
    }

    /// A changed filter picks its rows from the kept exact sort, and gets the rows an exact sort
    /// of the entries it passes gives, whether it narrows, widens or is cleared.
    #[test]
    fn refilter_gives_the_rows_of_an_exact_sort() {
        let es = ties(200);
        let shown = |_: usize, e: &EntryData| !e.hidden();
        let filters: [fn(usize) -> bool; 5] = [
            |i| i.is_multiple_of(2),
            |i| i.is_multiple_of(6),
            |i| i % 3 != 1,
            |_| false,
            |_| true,
        ];
        for key in [SortKey::Size, SortKey::Name, SortKey::Items, SortKey::Mtime] {
            for desc in [false, true] {
                let spec = SortSpec { key, desc };
                let mut o = Order::default();
                o.sort_exact_unfiltered(&es, shown, spec);
                for f in filters {
                    let visible = |i: usize, e: &EntryData| shown(i, e) && f(i);
                    assert!(o.refilter(&es, visible, spec));
                    let mut want = Order::default();
                    want.sort_exact(&es, visible, spec);
                    assert_eq!(o.rows, want.rows, "{spec:?}");
                }
                // A sort kept apart from the rows gives the same rows.
                let mut o = Order::default();
                o.sort_auto(&es, all, spec);
                let auto = o.rows.clone();
                o.keep_exact(&es, shown, spec);
                assert_eq!(o.rows, auto, "keeping a sort leaves the rows alone");
                let visible = |i: usize, e: &EntryData| shown(i, e) && i.is_multiple_of(2);
                assert!(o.refilter(&es, visible, spec));
                let mut want = Order::default();
                want.sort_exact(&es, visible, spec);
                assert_eq!(o.rows, want.rows, "{spec:?}");
            }
        }
    }

    /// Without an exact sort that still holds for the entries, `refilter` changes nothing, so
    /// the rows are sorted instead.
    #[test]
    fn refilter_needs_an_exact_sort_that_still_holds() {
        let mut es = ties(20);
        let spec = SortSpec::default();
        let refilters = |o: &mut Order, es: &[EntryData]| {
            let rows = o.rows.clone();
            let done = o.refilter(es, |i, _| i.is_multiple_of(2), spec);
            assert!(done || o.rows == rows);
            done
        };
        let mut o = Order::default();
        assert!(!refilters(&mut o, &es), "no sort yet");
        o.sort_exact(&es, |i, _| i.is_multiple_of(3), spec);
        assert!(!refilters(&mut o, &es), "the rows were filtered");
        o.sort_exact_unfiltered(&es, all, SortSpec::for_key(SortKey::Name));
        assert!(!refilters(&mut o, &es), "sorted by another key");
        o.sort_exact_unfiltered(&es, all, spec);
        o.sort_auto(&es, all, spec);
        assert!(!refilters(&mut o, &es), "sorted automatically");
        o.sort_exact_unfiltered(&es, all, spec);
        es[3].size = 1 << 30;
        es.push(test_entry(20, "late", 5));
        o.sync(&es, all, spec);
        assert!(!refilters(&mut o, &es), "sizes and entries changed");
        o.keep_exact(&es, all, spec);
        o.remap(Some);
        assert!(!refilters(&mut o, &es), "the entries were replaced");
        o.keep_exact(&es, all, spec);
        assert!(refilters(&mut o, &es));
        assert!(
            refilters(&mut o, &es),
            "the kept sort stays for the next change"
        );
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
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            (seed >> 33) % n
        };
        let mut es = Vec::new();
        for i in 0..300u32 {
            let name = format!(
                "{}{}",
                names[usize::try_from(next(names.len() as u64)).unwrap()],
                next(3)
            );
            let mut e = test_entry(
                i,
                &name,
                [0, 10, 1000, 1001, 5000][usize::try_from(next(5)).unwrap()],
            );
            e.items = next(4);
            e.mtime = i64::try_from(next(3)).unwrap() - 1;
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
                        base: Base::None,
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
                        if quantise {
                            bucket(v)
                        } else {
                            i64::try_from(v).unwrap()
                        }
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
