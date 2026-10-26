//! Growable storage in fixed-size pages. Growing never moves or copies what is already stored,
//! so adding one entry costs at most one page allocation. A plain `Vec` doubles and copies: at
//! 1M nodes that is a 32 MB copy inside one applier slice, several times the 1 ms a slice may
//! hold the write lock (`docs/data-pipeline.md` §5.6).

use std::ops::{Index, IndexMut};

const PAGE_SHIFT: u32 = 16;
const PAGE: usize = 1 << PAGE_SHIFT;
const MASK: usize = PAGE - 1;

/// An array of `T` in pages of 65,536 elements.
pub(crate) struct Pages<T> {
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
