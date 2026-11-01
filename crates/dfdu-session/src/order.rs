//! Row order of the current directory, kept stable while sizes stream in.
//!
//! The order is rebuilt only when asked: an exact sort on entering a directory, on a sort key
//! or on `S`, and an automatic sort after the user has been idle. Between sorts, rows keep
//! their positions, entries that disappeared are dropped, and entries that appeared are
//! collected as "new" and shown after the sorted rows. While the cursor is at the top, the
//! first screen of rows is kept sorted with each snapshot ([`Order::sort_top`]).
//!
//! In tree mode the rows are a preorder of the expanded tree, and the same rules apply within
//! each group of siblings (`tree`, [`Order::sort_tree`]). Entries that appear between sorts
//! then go after their known siblings instead of to the new list, and the first screen is not
//! sorted with each snapshot: a row moving there would carry its expanded children along.

mod tree;

use std::cmp::Ordering;

pub use tree::Siblings;
pub use tree::{tree_order, tree_rows};

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

    /// The rows in tree mode ([`tree_rows`]), with siblings ordered as `how` says. With no
    /// rows yet, siblings get an exact sort whatever `how` is. The entries must be the ones the
    /// order refers to ([`Order::remap`]).
    #[expect(
        clippy::cast_possible_truncation,
        reason = "positions are rows of one snapshot, fewer than 2^32"
    )]
    pub fn sort_tree(
        &mut self,
        entries: &[EntryData],
        visible: impl Fn(usize, &EntryData) -> bool,
        spec: SortSpec,
        how: Siblings,
    ) {
        let mut prev = Vec::new();
        if how != Siblings::Exact {
            prev = vec![u32::MAX; entries.len()];
            for (pos, &i) in self.rows.iter().chain(self.new.iter()).enumerate() {
                prev[i] = pos as u32;
            }
        }
        self.rows = tree_rows(entries, visible, spec, &prev, how);
        self.new.clear();
        self.base = Base::None;
    }

    /// Updates the order as [`Order::sync`] does, then sorts its first `height` rows: they become
    /// the `height` rows an automatic sort would put first, in its order, picked from the rows
    /// and the new entries. The other rows keep their order after them, and the other new
    /// entries stay new.
    #[expect(
        clippy::cast_possible_truncation,
        reason = "positions are rows of one directory, fewer than 2^32"
    )]
    pub fn sort_top(
        &mut self,
        entries: &[EntryData],
        visible: impl Fn(usize, &EntryData) -> bool,
        spec: SortSpec,
        height: usize,
    ) {
        self.sync(entries, visible, spec);
        if height == 0 {
            return;
        }
        let mut prev = vec![u32::MAX; entries.len()];
        for (pos, &i) in self.rows.iter().chain(self.new.iter()).enumerate() {
            prev[i] = pos as u32;
        }
        let candidates = self.rows.iter().chain(self.new.iter()).copied();
        let top = ranked(entries, candidates, spec, &prev, height);
        // `prev` is done with: mark the rows that went to the top.
        for &i in &top {
            prev[i] = u32::MAX;
        }
        let rest: Vec<usize> = self
            .rows
            .iter()
            .copied()
            .filter(|&i| prev[i] != u32::MAX)
            .collect();
        self.new.retain(|&i| prev[i] != u32::MAX);
        self.rows = top;
        self.rows.extend(rest);
    }
}

/// The entries `visible` passes, sorted by `spec`. `prev[i]`: the position of entry `i` before
/// an automatic sort, used as a tie-break (missing: after all others); empty for an exact sort.
fn sorted(
    entries: &[EntryData],
    visible: impl Fn(usize, &EntryData) -> bool,
    spec: SortSpec,
    prev: &[u32],
) -> Vec<usize> {
    let shown = entries
        .iter()
        .enumerate()
        .filter(|&(i, e)| visible(i, e))
        .map(|(i, _)| i);
    ranked(entries, shown, spec, prev, usize::MAX)
}

/// The first `limit` of the entries `indices`, in the order [`sorted`] gives them.
#[expect(
    clippy::cast_possible_truncation,
    reason = "keys hold entry indices in u32 to stay small; a directory has fewer than 2^32 entries"
)]
fn ranked(
    entries: &[EntryData],
    indices: impl Iterator<Item = usize>,
    spec: SortSpec,
    prev: &[u32],
    limit: usize,
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
    let mut keys: Vec<Key> = indices
        .map(|i| {
            let e = &entries[i];
            Key {
                first: first(e),
                prev: prev.get(i).copied().unwrap_or(u32::MAX),
                prefix: name_prefix(&e.name),
                index: i as u32,
            }
        })
        .collect();
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
    if keys.len() > limit {
        if limit == 0 {
            return Vec::new();
        }
        // Only the first `limit` are wanted: split the keys at the last of them, and keep with
        // the first part the keys that the integer key cannot tell from it, since the full
        // comparison below may put them first.
        keys.select_nth_unstable(limit - 1);
        let (head, tail) = keys.split_at_mut(limit);
        let edge = &head[limit - 1];
        let mut n = 0;
        for j in 0..tail.len() {
            if undecided(edge, &tail[j]) {
                tail.swap(n, j);
                n += 1;
            }
        }
        keys.truncate(limit + n);
    }
    keys.sort_unstable();
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
    keys.truncate(limit);
    keys.into_iter().map(|k| k.index as usize).collect()
}

#[cfg(test)]
mod tests;
