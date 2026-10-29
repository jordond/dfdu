//! A directory's entries classified together: the rules and protected paths that can match
//! below it are picked once per listing, then each entry is checked against those.

use super::{Candidate, Hit, RuleSet, Verdict};
use crate::{Glob, RuleIx, glob, protected};

#[cfg(test)]
mod tests;

/// What a directory's entries are classified against: the rules and protected paths that can
/// match below `parent`, found once per listing so that each entry costs little.
pub struct Dir<'a> {
    set: &'a RuleSet,
    parent: &'a [u8],
    /// Indices into `RuleSet::paths` whose pattern can match an entry here.
    paths: Vec<usize>,
    /// The sibling tails ([`RuleSet::sibling_tails`]) of the rules that can match an entry
    /// here. `{name_stem}` leaves an empty tail, which every name ends with, so a directory
    /// where no such rule can match must not ask for it.
    tails: Vec<&'a [u8]>,
    protected: protected::Dir,
    reach: Reach,
}

/// The path patterns, of rules and of protected paths, that can match an entry of a directory
/// or anything below one. A listing hands its own to its subdirectories ([`Dir::reach`]), so
/// a directory deep in a tree looks only at the few patterns that can reach it, usually the
/// ones starting with `**`, instead of every pattern.
#[derive(Clone, Debug, Default)]
pub struct Reach {
    /// Indices into `RuleSet::paths`.
    paths: Vec<u32>,
    protected: protected::Reach,
}

/// `parent` with a `/` at the end.
fn with_slash(parent: &[u8]) -> Vec<u8> {
    let mut p = parent.to_vec();
    if !p.ends_with(b"/") {
        p.push(b'/');
    }
    p
}

/// The number of components of an entry of the directory whose path (with a `/` at the end)
/// is `dir`, for [`may_match_below`].
#[expect(
    clippy::naive_bytecount,
    reason = "patterns and paths are short; not worth the `bytecount` dependency"
)]
pub(crate) fn depth_below(dir: &[u8]) -> usize {
    dir.iter().filter(|&&b| b == b'/').count() + 1
}

/// Whether glob `g` can match an entry of the directory whose path (with a `/` at the end)
/// is `dir`, whose entries have `depth` components ([`depth_below`]), and whether it can match
/// one of them or anything below them: one of the two paths starts with the other, and a
/// pattern without `**` has that many components, or at least that many.
pub(crate) fn may_match_below(g: &Glob, dir: &[u8], depth: usize, fold: bool) -> (bool, bool) {
    let prefix = g.prefix();
    let related = glob::starts_with(dir, prefix, fold)
        || (prefix.len() >= dir.len() && glob::starts_with(dir, &prefix[..dir.len()], fold));
    match g.depth() {
        _ if !related => (false, false),
        None => (true, true),
        Some(d) => (d == depth, d >= depth),
    }
}

impl RuleSet {
    /// The context for classifying the entries of the directory at `parent`.
    #[must_use]
    pub fn dir<'a>(&'a self, parent: &'a [u8]) -> Dir<'a> {
        self.dir_in(parent, None)
    }

    /// [`RuleSet::dir`] trying only the patterns that reach `parent`: those its parent's
    /// listing found ([`Dir::reach`]), or all of them with `None`.
    #[must_use]
    pub fn dir_in<'a>(&'a self, parent: &'a [u8], reach: Option<&Reach>) -> Dir<'a> {
        let dir = with_slash(parent);
        let depth = depth_below(&dir);
        let candidates: Box<dyn Iterator<Item = u32>> = match reach {
            Some(r) => Box::new(r.paths.iter().copied()),
            #[expect(
                clippy::cast_possible_truncation,
                reason = "rule files cannot hold u32::MAX path patterns: each is a compiled glob"
            )]
            None => Box::new(0..self.paths.len() as u32),
        };
        let (mut paths, mut below) = (Vec::new(), Vec::new());
        for k in candidates {
            let (here, deeper) = may_match_below(&self.paths[k as usize].1, &dir, depth, self.fold);
            if here {
                paths.push(k as usize);
            }
            if deeper {
                below.push(k);
            }
        }
        let (protected, reach_protected) = self.protected.dir_in(&dir, reach.map(|r| &r.protected));
        let mut tails: Vec<&[u8]> = Vec::new();
        for (t, i) in &self.sibling_tails {
            let here =
                self.rule(*i).paths.is_empty() || paths.iter().any(|&k| self.paths[k].0 == *i);
            if here && !tails.contains(&&t[..]) {
                tails.push(t);
            }
        }
        Dir {
            set: self,
            parent,
            paths,
            tails,
            protected,
            reach: Reach {
                paths: below,
                protected: reach_protected,
            },
        }
    }
}

impl Dir<'_> {
    /// The patterns that can reach this directory's subdirectories, for [`RuleSet::dir_in`].
    #[must_use]
    pub fn reach(&self) -> &Reach {
        &self.reach
    }

    /// Whether a rule that can match an entry here may ask whether an entry named `name` is
    /// next to its candidate. A directory listed in several blocks keeps only these names for
    /// [`RuleSet::siblings`].
    #[must_use]
    pub fn may_be_sibling(&self, name: &[u8]) -> bool {
        self.set.sibling_names.contains(name) || self.tails.iter().any(|t| name.ends_with(t))
    }

    /// Classifies entry `c` of this directory (`c.parent` is ignored).
    pub fn classify(&self, c: &Candidate) -> Verdict {
        let set = self.set;
        // The full path, built only for the checks that need it.
        let path = || {
            let mut p = with_slash(self.parent);
            p.extend_from_slice(c.name);
            p
        };
        let state = set
            .protected
            .step_in(&self.protected, c.protection, c.name, &path);
        if state.protected() {
            return Verdict::Protected(state);
        }
        if c.inside.iter().any(|&r| set.rule(r).stop_descent) {
            return Verdict::None;
        }
        let mut found: Vec<RuleIx> = Vec::new();
        if c.is_dir || set.file_names {
            let rules = if set.fold && c.name.iter().any(u8::is_ascii_uppercase) {
                set.by_name.get(&c.name.to_ascii_lowercase())
            } else {
                set.by_name.get(c.name)
            };
            if let Some(rules) = rules {
                found.extend(rules);
            }
            for &i in &set.name_globs {
                if !found.contains(&i) && set.rule(i).names.iter().any(|g| g.matches(c.name)) {
                    found.push(i);
                }
            }
        }
        if !self.paths.is_empty() {
            let full = std::cell::OnceCell::new();
            for &k in &self.paths {
                let (i, g) = &set.paths[k];
                let r = set.rule(*i);
                if found.contains(i) || !set.fits(r, c) {
                    continue;
                }
                let name_ok = r.names.is_empty() || r.names.iter().any(|n| n.matches(c.name));
                if name_ok && g.matches(full.get_or_init(path)) {
                    found.push(*i);
                }
            }
        }
        let mut hits: Vec<Hit> = found
            .into_iter()
            .filter_map(|i| set.check(i, c, &path))
            .collect();
        if hits.is_empty() {
            return Verdict::None;
        }
        hits.sort_by_key(|h| {
            let r = set.rule(h.rule);
            let conditions = r.sibling.len() + r.child.len() + r.markers.len();
            (
                std::cmp::Reverse(r.priority),
                std::cmp::Reverse(conditions),
                h.rule,
            )
        });
        Verdict::Hits(hits)
    }
}
