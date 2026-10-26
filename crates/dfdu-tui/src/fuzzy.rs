//! Fuzzy matching with nucleo, for the filter, the palette and the help search. Each word of the
//! query must match (in any order); a word with an upper-case letter matches case, others do
//! not, and a word without accents also matches accented letters.

#![allow(
    dead_code,
    reason = "used by the filter, the palette and the help search"
)]

use nucleo_matcher::pattern::{Atom, AtomKind, CaseMatching, Normalization};
use nucleo_matcher::{Config, Matcher, Utf32Str};

/// A query with the matcher and buffer it reuses between calls, so matching a million names
/// allocates nothing per name.
#[derive(Clone, Debug, Default)]
pub struct Fuzzy {
    atoms: Vec<Atom>,
    matcher: Matcher,
    chars: Vec<char>,
}

impl Fuzzy {
    pub fn new(query: &str) -> Self {
        Fuzzy {
            atoms: query
                .split_whitespace()
                .map(|w| {
                    Atom::new(
                        w,
                        CaseMatching::Smart,
                        Normalization::Smart,
                        AtomKind::Fuzzy,
                        false,
                    )
                })
                .collect(),
            matcher: Matcher::new(Config::DEFAULT),
            chars: Vec::new(),
        }
    }

    /// True for a query without words, which matches everything.
    pub fn is_empty(&self) -> bool {
        self.atoms.is_empty()
    }

    /// The score of `text` (higher is better), or `None` if it does not match.
    pub fn score(&mut self, text: &str) -> Option<u32> {
        let haystack = utf32(text, &mut self.chars);
        let mut total = 0;
        for atom in &self.atoms {
            total += u32::from(atom.score(haystack, &mut self.matcher)?);
        }
        Some(total)
    }

    /// Like [`Fuzzy::score`], and sets `out` to the indices of the matched characters of
    /// `text` (counted in `char`s), sorted and without duplicates.
    pub fn indices(&mut self, text: &str, out: &mut Vec<u32>) -> Option<u32> {
        out.clear();
        let haystack = utf32(text, &mut self.chars);
        let mut total = 0;
        for atom in &self.atoms {
            match atom.indices(haystack, &mut self.matcher, out) {
                Some(s) => total += u32::from(s),
                None => {
                    out.clear();
                    return None;
                }
            }
        }
        out.sort_unstable();
        out.dedup();
        Some(total)
    }
}

/// `text` as nucleo reads it: its bytes when it is ASCII, otherwise one `char` per character
/// (so match indices are `char` indices).
fn utf32<'a>(text: &'a str, buf: &'a mut Vec<char>) -> Utf32Str<'a> {
    if text.is_ascii() {
        Utf32Str::Ascii(text.as_bytes())
    } else {
        buf.clear();
        buf.extend(text.chars());
        Utf32Str::Unicode(buf)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_subsequences() {
        let mut f = Fuzzy::new("srtsz");
        assert!(f.score("sort by size").is_some());
        assert!(f.score("size sort").is_none(), "letters must come in order");
        // Words match independently, in any order.
        let mut f = Fuzzy::new("sz srt");
        assert!(f.score("sort by size").is_some());
        assert!(f.score("sort by name").is_none());
        // A closer match scores higher.
        let mut f = Fuzzy::new("node");
        assert!(f.score("node_modules") > f.score("n_o_d_e"));
    }

    #[test]
    fn smart_case() {
        let mut lower = Fuzzy::new("readme");
        assert!(lower.score("README.md").is_some());
        assert!(lower.score("readme.md").is_some());
        let mut upper = Fuzzy::new("README");
        assert!(upper.score("README.md").is_some());
        assert!(upper.score("readme.md").is_none());
        // Without accents in the query, accented names match too.
        let mut plain = Fuzzy::new("cafe");
        assert!(plain.score("Café").is_some());
    }

    #[test]
    fn indices_point_at_matched_chars() {
        let mut f = Fuzzy::new("lib");
        let mut out = vec![99];
        assert!(f.indices("Library", &mut out).is_some());
        assert_eq!(out, [0, 1, 2]);
        // Indices count characters, not bytes.
        let mut f = Fuzzy::new("ab");
        assert!(f.indices("éaéb", &mut out).is_some());
        assert_eq!(out, [1, 3]);
        assert!(f.indices("xyz", &mut out).is_none());
        assert!(out.is_empty());
    }

    #[test]
    fn empty_query_matches_everything() {
        for q in ["", "   "] {
            let mut f = Fuzzy::new(q);
            assert!(f.is_empty());
            assert_eq!(f.score("anything"), Some(0));
            assert_eq!(f.score(""), Some(0));
            let mut out = Vec::new();
            assert_eq!(f.indices("x", &mut out), Some(0));
            assert!(out.is_empty());
        }
        assert!(!Fuzzy::new("a").is_empty());
    }
}
