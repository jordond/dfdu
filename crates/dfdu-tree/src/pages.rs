//! Growable storage in fixed-size pages. Growing never moves or copies what is already stored,
//! so adding one entry costs at most one page allocation. A plain `Vec` doubles and copies: at
//! 1M nodes that is a 32 MB copy inside one applier slice, several times the 1 ms a slice may
//! hold the write lock (`docs/data-pipeline.md` §5.6). Nodes, directory records, child lists
//! and names are stored this way, and the applier's per-job directory maps.

use std::ops::{Index, IndexMut};

const PAGE_SHIFT: u32 = 16;
const PAGE: usize = 1 << PAGE_SHIFT;
const MASK: usize = PAGE - 1;

/// An array of `T` in pages of 65,536 elements. A page's memory is reserved when the page is
/// started and touched as it fills.
pub struct Pages<T> {
    pages: Vec<Vec<T>>,
    len: usize,
}

impl<T> Default for Pages<T> {
    fn default() -> Self {
        Pages {
            pages: Vec::new(),
            len: 0,
        }
    }
}

impl<T> Pages<T> {
    pub fn len(&self) -> usize {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    pub fn push(&mut self, v: T) {
        if self.len & MASK == 0 {
            self.pages.push(Vec::with_capacity(PAGE));
        }
        self.pages.last_mut().expect("a page").push(v);
        self.len += 1;
    }

    pub fn get(&self, i: usize) -> Option<&T> {
        (i < self.len).then(|| &self.pages[i >> PAGE_SHIFT][i & MASK])
    }

    /// Extends to `n` elements with copies of `v` (never shrinks).
    pub fn grow_to(&mut self, n: usize, v: T)
    where
        T: Clone,
    {
        while self.len < n {
            self.push(v.clone());
        }
    }
}

impl<T> Index<usize> for Pages<T> {
    type Output = T;
    fn index(&self, i: usize) -> &T {
        assert!(i < self.len, "index {i} out of {}", self.len);
        &self.pages[i >> PAGE_SHIFT][i & MASK]
    }
}

impl<T> IndexMut<usize> for Pages<T> {
    fn index_mut(&mut self, i: usize) -> &mut T {
        assert!(i < self.len, "index {i} out of {}", self.len);
        &mut self.pages[i >> PAGE_SHIFT][i & MASK]
    }
}

const KID_PAGE_BITS: u32 = 16;
const KID_PAGE: usize = 1 << KID_PAGE_BITS;

/// Every directory's child list. A directory's first chunk of children goes into a segment of
/// a page shared with other directories, sized for that chunk: most directories arrive in one
/// chunk, and a `Vec` per directory cost a `malloc` each (about 1 µs each in the applier's
/// fragmented heap at 10M entries). When the segment is full, later children go into blocks of
/// the directory's own, each at least as large as the list before it, up to a page. Growing
/// never moves what is stored, so the lock is never held for a copy of a wide directory's list
/// (a `Vec` of 10M entries doubling copied 40 MB).
///
/// Segments of freed directories are not reused, like names ([`NameArena`]).
#[derive(Default)]
pub(crate) struct KidArena {
    pages: Vec<Vec<u32>>,
    garbage: u64,
}

/// One directory's child list in a [`KidArena`]: a segment of `len` of `cap` slots at `pos`
/// (page and offset), then the blocks in `more`, in order.
#[derive(Clone, Debug, Default)]
pub(crate) struct Kids {
    pos: u32,
    len: u32,
    cap: u32,
    /// Boxed: 8 bytes in every directory record instead of 24, and only directories of
    /// several chunks have blocks.
    #[allow(clippy::box_collection)]
    more: Option<Box<Vec<Vec<u32>>>>,
}

impl Kids {
    fn blocks(&self) -> &[Vec<u32>] {
        self.more.as_deref().map_or(&[], |m| &m[..])
    }

    pub fn len(&self) -> usize {
        self.len as usize + self.blocks().iter().map(Vec::len).sum::<usize>()
    }

    /// Drops the unused room at the end of the last block (a wide directory's last chunk).
    pub fn shrink(&mut self) {
        if let Some(b) = self.more.as_mut().and_then(|m| m.last_mut()) {
            b.shrink_to_fit();
        }
    }
}

impl KidArena {
    fn page_off(k: &Kids) -> (usize, usize) {
        (
            (k.pos >> KID_PAGE_BITS) as usize,
            (k.pos as usize) & (KID_PAGE - 1),
        )
    }

    /// The children: the segment, then the blocks.
    pub fn parts<'a>(&'a self, k: &'a Kids) -> (&'a [u32], &'a [Vec<u32>]) {
        let seg = if k.cap == 0 {
            &[][..]
        } else {
            let (p, o) = Self::page_off(k);
            &self.pages[p][o..o + k.len as usize]
        };
        (seg, k.blocks())
    }

    fn seg_mut(&mut self, k: &Kids) -> &mut [u32] {
        if k.cap == 0 {
            return &mut [];
        }
        let (p, o) = Self::page_off(k);
        &mut self.pages[p][o..o + k.cap as usize]
    }

    /// Room for `n` more children.
    pub fn reserve(&mut self, k: &mut Kids, n: usize) {
        let free = match k.more.as_deref().and_then(|m| m.last()) {
            Some(b) => b.capacity() - b.len(),
            None => (k.cap - k.len) as usize,
        };
        if n <= free {
            return;
        }
        if k.cap == 0 && k.more.is_none() && n <= KID_PAGE {
            let fits = self.pages.last().is_some_and(|p| KID_PAGE - p.len() >= n);
            if !fits {
                assert!(
                    self.pages.len() < 1 << (32 - KID_PAGE_BITS),
                    "fewer than 2^32 children"
                );
                self.pages.push(Vec::with_capacity(KID_PAGE));
            }
            let page = self.pages.len() - 1;
            let p = &mut self.pages[page];
            k.pos = (page << KID_PAGE_BITS | p.len()) as u32;
            k.cap = n as u32;
            p.resize(p.len() + n, 0);
            return;
        }
        let size = n.max(k.len().clamp(1, KID_PAGE));
        k.more
            .get_or_insert_default()
            .push(Vec::with_capacity(size));
    }

    pub fn push(&mut self, k: &mut Kids, c: u32) {
        self.reserve(k, 1);
        match k.more.as_deref_mut().and_then(|m| m.last_mut()) {
            Some(b) => b.push(c),
            None => {
                let i = k.len as usize;
                self.seg_mut(k)[i] = c;
                k.len += 1;
            }
        }
    }

    /// Removes the last child.
    pub fn pop(&mut self, k: &mut Kids) -> Option<u32> {
        while let Some(m) = k.more.as_deref_mut() {
            match m.last_mut().and_then(Vec::pop) {
                Some(c) => return Some(c),
                None => {
                    m.pop();
                    if m.is_empty() {
                        k.more = None;
                    }
                }
            }
        }
        if k.len == 0 {
            return None;
        }
        k.len -= 1;
        Some(self.seg_mut(k)[k.len as usize])
    }

    /// Removes `c`, keeping the order of the others. Returns false if it is not a child.
    pub fn remove(&mut self, k: &mut Kids, c: u32) -> bool {
        let len = k.len as usize;
        let seg = self.seg_mut(k);
        if let Some(i) = seg[..len].iter().position(|&x| x == c) {
            seg.copy_within(i + 1..len, i);
            k.len -= 1;
            return true;
        }
        for b in k.more.iter_mut().flat_map(|m| m.iter_mut()) {
            if let Some(i) = b.iter().position(|&x| x == c) {
                b.remove(i);
                return true;
            }
        }
        false
    }

    /// Puts `new` in the place of `old`. Returns false if `old` is not a child.
    pub fn replace(&mut self, k: &mut Kids, old: u32, new: u32) -> bool {
        let len = k.len as usize;
        let seg = &mut self.seg_mut(k)[..len];
        let slot = seg.iter_mut().find(|x| **x == old).or_else(|| {
            k.more
                .iter_mut()
                .flat_map(|m| m.iter_mut())
                .flat_map(|b| b.iter_mut())
                .find(|x| **x == old)
        });
        slot.map(|x| *x = new).is_some()
    }

    /// Frees the list; its segment's slots are not reused.
    pub fn free(&mut self, k: &mut Kids) {
        self.garbage += k.cap as u64;
        *k = Kids::default();
    }

    /// Slots in pages, and how many of them freed lists left unused.
    pub fn slots(&self) -> (u64, u64) {
        let used = self.pages.iter().map(|p| p.len() as u64).sum();
        (used, self.garbage)
    }
}

const NAME_PAGE_BITS: u32 = 20;
const NAME_PAGE: usize = 1 << NAME_PAGE_BITS;
const NAME_OFF_BITS: u32 = 20;
const NAME_LEN_BITS: u32 = 24;

/// Names in pages of 1 MiB. A name never spans two pages; one longer than a page gets a page
/// of its own. A name is found by a handle packing its page (20 bits), offset in the page (20
/// bits) and length (24 bits): up to 2^20 pages, and names up to 16 MiB (root paths can be
/// long).
#[derive(Default)]
pub(crate) struct NameArena {
    pages: Vec<Vec<u8>>,
    bytes: u64,
}

impl NameArena {
    pub fn push(&mut self, name: &[u8]) -> u64 {
        assert!(name.len() < 1 << NAME_LEN_BITS, "name shorter than 16 MiB");
        let fits = self
            .pages
            .last()
            .is_some_and(|p| p.capacity() - p.len() >= name.len());
        if !fits {
            self.pages
                .push(Vec::with_capacity(NAME_PAGE.max(name.len())));
        }
        assert!(
            self.pages.len() <= 1 << NAME_PAGE_BITS,
            "names shorter than 1 TiB in all"
        );
        let page = self.pages.len() - 1;
        let p = &mut self.pages[page];
        let off = p.len();
        p.extend_from_slice(name);
        self.bytes += name.len() as u64;
        (page as u64) << (NAME_OFF_BITS + NAME_LEN_BITS)
            | (off as u64) << NAME_LEN_BITS
            | name.len() as u64
    }

    pub fn get(&self, h: u64) -> &[u8] {
        let len = (h & ((1 << NAME_LEN_BITS) - 1)) as usize;
        let off = ((h >> NAME_LEN_BITS) & ((1 << NAME_OFF_BITS) - 1)) as usize;
        let page = (h >> (NAME_OFF_BITS + NAME_LEN_BITS)) as usize;
        &self.pages[page][off..off + len]
    }

    /// Length of the name a handle refers to.
    pub fn len_of(h: u64) -> u64 {
        h & ((1 << NAME_LEN_BITS) - 1)
    }

    /// Bytes stored, freed names included.
    pub fn bytes(&self) -> u64 {
        self.bytes
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pages_index_across_page_boundaries() {
        let mut p: Pages<u32> = Pages::default();
        for i in 0..(3 * PAGE + 5) as u32 {
            p.push(i);
        }
        assert_eq!(p.len(), 3 * PAGE + 5);
        for i in [0, PAGE - 1, PAGE, 2 * PAGE + 1, 3 * PAGE + 4] {
            assert_eq!(p[i], i as u32);
        }
        assert!(p.get(3 * PAGE + 5).is_none());
        p[PAGE] = 7;
        assert_eq!(p[PAGE], 7);
        p.grow_to(4 * PAGE, 9);
        assert_eq!((p.len(), p[4 * PAGE - 1]), (4 * PAGE, 9));
    }

    /// Child lists behave as `Vec`s do under the applier's operations, with several lists
    /// sharing pages, lists growing past a page, and lists freed and started again.
    #[test]
    fn child_lists_match_vecs() {
        let mut a = KidArena::default();
        let mut lists: Vec<(Kids, Vec<u32>)> = (0..40).map(|_| Default::default()).collect();
        let mut x: u64 = 0x9e3779b97f4a7c15;
        let mut rnd = |n: u64| {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            x % n
        };
        let mut next = 0u32;
        for _ in 0..5_000 {
            let i = rnd(lists.len() as u64) as usize;
            let (k, v) = &mut lists[i];
            match rnd(10) {
                0..=4 => {
                    // A chunk: reserve, then push.
                    let n = match rnd(100) {
                        0 => 3 * KID_PAGE as u64 / 2,
                        1..=4 => rnd(5000),
                        _ => rnd(40),
                    } as usize;
                    a.reserve(k, n);
                    for _ in 0..n {
                        a.push(k, next);
                        v.push(next);
                        next += 1;
                    }
                }
                5 => {
                    a.push(k, next);
                    v.push(next);
                    next += 1;
                }
                6 if !v.is_empty() => {
                    let c = v.remove(rnd(v.len() as u64) as usize);
                    assert!(a.remove(k, c));
                }
                7 if !v.is_empty() => {
                    let j = rnd(v.len() as u64) as usize;
                    assert!(a.replace(k, v[j], next));
                    v[j] = next;
                    next += 1;
                }
                8 => {
                    for _ in 0..rnd(100) {
                        assert_eq!(a.pop(k), v.pop());
                    }
                }
                _ if rnd(20) == 0 => {
                    a.free(k);
                    v.clear();
                }
                _ => k.shrink(),
            }
            assert!(!a.remove(k, u32::MAX));
            let (seg, more) = a.parts(k);
            let got: Vec<u32> = seg.iter().chain(more.iter().flatten()).copied().collect();
            assert_eq!(&got, v);
            assert_eq!(k.len(), v.len());
        }
        let (slots, garbage) = a.slots();
        assert!(garbage <= slots);
    }

    #[test]
    fn names_round_trip_including_long_ones() {
        let mut a = NameArena::default();
        let long = vec![b'x'; NAME_PAGE + 10];
        let mut handles = vec![];
        for i in 0..200_000u32 {
            let n = format!("name-{i}");
            handles.push((a.push(n.as_bytes()), n.into_bytes()));
            if i == 100_000 {
                handles.push((a.push(&long), long.clone()));
            }
        }
        handles.push((a.push(b""), vec![]));
        for (h, n) in &handles {
            assert_eq!(a.get(*h), &n[..]);
            assert_eq!(NameArena::len_of(*h), n.len() as u64);
        }
    }
}
