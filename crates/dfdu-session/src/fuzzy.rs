//! Fuzzy matching with nucleo, for the filter, the palette and the help search. Each word of the
//! query must match (in any order); a word with an upper-case letter matches case, others do
//! not, and a word without accents also matches accented letters.

use nucleo_matcher::pattern::{Atom, AtomKind, CaseMatching, Normalization};
use nucleo_matcher::{Config, Matcher, Utf32Str};

/// Character position `i` of a text as matches count them. Texts are names and labels, far
/// shorter than `u32::MAX` characters.
pub fn char_pos(i: usize) -> u32 {
    u32::try_from(i).unwrap_or(u32::MAX)
}

/// A query with the matcher and buffer it reuses between calls, so matching a million names
/// allocates nothing per name.
#[derive(Clone, Debug, Default)]
pub struct Fuzzy {
    atoms: Vec<Atom>,
    /// Per atom, the case and normalization settings it matches with.
    configs: Vec<(bool, bool)>,
    matcher: Matcher,
    chars: Vec<char>,
}

impl Fuzzy {
    pub fn new(query: &str) -> Self {
        let atoms: Vec<Atom> = query
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
            .collect();
        let mut matcher = Matcher::new(Config::DEFAULT);
        // Scoring sets the matcher up the way the atom matches.
        let configs = atoms
            .iter()
            .map(|a| {
                a.score(Utf32Str::Ascii(b""), &mut matcher);
                (matcher.config.ignore_case, matcher.config.normalize)
            })
            .collect();
        Fuzzy {
            atoms,
            configs,
            matcher,
            chars: Vec::new(),
        }
    }

    /// True for a query without words, which matches everything.
    pub fn is_empty(&self) -> bool {
        self.atoms.is_empty()
    }

    /// The score of `text` (higher is better), or `None` if it does not match, as
    /// [`Fuzzy::indices`] returns it.
    #[cfg(test)]
    pub fn score(&mut self, text: &str) -> Option<u32> {
        let haystack = utf32(text, &mut self.chars);
        let mut total = 0;
        for atom in &self.atoms {
            total += u32::from(atom.score(haystack, &mut self.matcher)?);
        }
        Some(total)
    }

    /// Whether `text` matches, as [`Fuzzy::indices`] decides it but faster: a greedy match that
    /// nothing is scored for. It agrees with nucleo's scoring except for rare case-folding
    /// cases, such as Greek final sigma (`σ` in the query, `ς` in the name), where it may
    /// report a match that scoring does not find.
    pub fn matches(&mut self, text: &str) -> bool {
        let haystack = utf32(text, &mut self.chars);
        self.atoms
            .iter()
            .zip(&self.configs)
            .all(|(atom, &(case, norm))| {
                self.matcher.config.ignore_case = case;
                self.matcher.config.normalize = norm;
                self.matcher
                    .fuzzy_match_greedy(haystack, atom.needle_text())
                    .is_some()
            })
    }

    /// The score of `text` (higher is better), or `None` if it does not match. Sets `out` to
    /// the indices of the matched characters of `text` (counted in `char`s), sorted and
    /// without duplicates.
    pub fn indices(&mut self, text: &str, out: &mut Vec<u32>) -> Option<u32> {
        out.clear();
        let haystack = utf32(text, &mut self.chars);
        let mut total = 0;
        for atom in &self.atoms {
            if let Some(s) = atom.indices(haystack, &mut self.matcher, out) {
                total += u32::from(s);
            } else {
                out.clear();
                return None;
            }
        }
        out.sort_unstable();
        out.dedup();
        Some(total)
    }
}

/// The `labels` that `query` matches, as the palette ranks them: by score, ties in the order
/// given. Each is its index in `labels` with the indices of its matched characters.
pub fn rank<'a>(query: &str, labels: impl IntoIterator<Item = &'a str>) -> Vec<(usize, Vec<u32>)> {
    let mut fuzzy = Fuzzy::new(query);
    let mut ranked: Vec<(u32, usize, Vec<u32>)> = Vec::new();
    for (i, label) in labels.into_iter().enumerate() {
        let mut indices = Vec::new();
        if let Some(score) = fuzzy.indices(label, &mut indices) {
            ranked.push((score, i, indices));
        }
    }
    ranked.sort_by_key(|&(score, i, _)| (std::cmp::Reverse(score), i));
    ranked.into_iter().map(|(_, i, ix)| (i, ix)).collect()
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
        assert_eq!(out, [] as [u32; 0]);
    }

    #[test]
    fn matches_agrees_with_score() {
        let names = [
            "sort by size",
            "README.md",
            "readme.md",
            "Café",
            "cafe\u{fffd}.txt",
            "node_modules",
            "n_o_d_e",
            "日本語のファイル",
            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaab",
            "",
        ];
        let queries = [
            "s",
            "srt sz",
            "sz srt",
            "README",
            "readme",
            "Readme",
            "cafe",
            "café",
            "\u{fffd}",
            "node",
            "日本",
            "ファ イル",
            "ab",
            "aab",
            "ba",
            "zz",
        ];
        for q in queries {
            let mut f = Fuzzy::new(q);
            for n in names {
                assert_eq!(f.matches(n), f.score(n).is_some(), "{q:?} in {n:?}");
            }
        }
        // The known exception, a case-folding quirk with a final sigma `ς` against `σ`:
        // scoring finds no match here but the greedy one does, so the row shows without a
        // highlight. If this fails, nucleo changed: drop the exception from the doc of
        // `Fuzzy::matches`.
        let mut f = Fuzzy::new("éσ");
        assert!(f.matches("Éeς"));
        assert_eq!(f.score("Éeς"), None);
        assert_eq!(f.indices("Éeς", &mut Vec::new()), None);
    }

    #[test]
    fn empty_query_matches_everything() {
        for q in ["", "   "] {
            let mut f = Fuzzy::new(q);
            assert!(f.is_empty());
            assert_eq!(f.score("anything"), Some(0));
            assert!(f.matches("anything"));
            assert_eq!(f.score(""), Some(0));
            let mut out = Vec::new();
            assert_eq!(f.indices("x", &mut out), Some(0));
            assert_eq!(out, [] as [u32; 0]);
        }
        assert!(!Fuzzy::new("a").is_empty());
    }

    #[test]
    fn rank_puts_the_best_first_and_keeps_the_order_of_ties() {
        let labels = ["sort by name", "size", "sort by size", "quit"];
        let ranked = rank("size", labels);
        let order: Vec<usize> = ranked.iter().map(|&(i, _)| i).collect();
        assert_eq!(order, [1, 2]);
        assert_eq!(ranked[0].1, [0, 1, 2, 3]);
        let all: Vec<usize> = rank("", labels).into_iter().map(|(i, _)| i).collect();
        assert_eq!(
            all,
            [0, 1, 2, 3],
            "an empty query keeps every label, in order"
        );
    }
}
