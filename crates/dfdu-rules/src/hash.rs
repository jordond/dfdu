//! A fast hash (FNV-1a) for the short byte strings looked up for every entry: names and
//! paths. The keys of these maps are the rules' own names and paths, fixed when the rule set
//! is built, so a crafted file name cannot make a lookup slow.

use std::collections::{HashMap, HashSet};
use std::hash::{BuildHasherDefault, Hasher};

#[derive(Clone, Copy)]
pub struct Fnv(u64);

impl Default for Fnv {
    fn default() -> Self {
        Fnv(0xcbf2_9ce4_8422_2325)
    }
}

impl Hasher for Fnv {
    fn finish(&self) -> u64 {
        self.0
    }

    fn write(&mut self, bytes: &[u8]) {
        for &b in bytes {
            self.0 = (self.0 ^ u64::from(b)).wrapping_mul(0x0100_0000_01b3);
        }
    }
}

pub type FastMap<K, V> = HashMap<K, V, BuildHasherDefault<Fnv>>;
pub type FastSet<K> = HashSet<K, BuildHasherDefault<Fnv>>;
