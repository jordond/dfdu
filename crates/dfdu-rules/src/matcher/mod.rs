//! Matching entries against a rule set (`categories-and-cleanup.md §9.2`, evaluation order).
//!
//! [`Dir::classify`] runs for each entry of a directory listing, with the listing at hand for
//! `sibling` checks: the protected-path check, then the name and path prefilter, then
//! `exclude`, `not_inside`, `sibling` and `degrade`. It must stay cheap, since it runs on the
//! applier: a name rule is found by a hash lookup, and the path patterns that can match in a
//! directory are picked once per listing ([`RuleSet::dir`]), so most entries never build their
//! full path. Conditions on what is inside the entry (`child`,
//! required `marker`) are left to [`RuleSet::check_inside`], called once the entry's own
//! listing is known. A file's own size and age (`larger_than`, `not_modified_days`) are
//! checked here from its stat; a rule's `min_size`, `min_age_days` and guards are checked by
//! the cleanup step.

use std::borrow::Cow;

use crate::hash::{FastMap, FastSet};
use crate::protected::{self, Protected};
use crate::vars::Env;
use crate::{Glob, Kind, Rule, Safety, glob};

/// An entry to classify: its directory's path (absolute, `/`-separated), its name, and the
/// rest of the listing.
pub struct Candidate<'a> {
    pub parent: &'a [u8],
    pub name: &'a [u8],
    pub is_dir: bool,
    /// True when the listing of `parent` has an entry with this name.
    pub sibling: &'a dyn Fn(&[u8]) -> bool,
    /// Rules matched by the directories above, innermost last.
    pub inside: &'a [u16],
    /// Whether the directory is protected (from its own [`Verdict::Protected`]).
    pub protection: protected::State,
    /// `sibling` knows only part of the listing (a wide directory read in several blocks), so
    /// a sibling it does not find may still be there: such a hit is marked [`Hit::open`].
    pub partial: bool,
    /// A file's stat, for `larger_than` and `not_modified_days`; `None` for a directory or
    /// when not known, which fails those conditions.
    pub stat: Option<Stat>,
}

/// What a rule may ask of a file's own metadata.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct Stat {
    /// Bytes allocated on disk.
    pub disk: u64,
    /// Modification time, seconds since the Unix epoch.
    pub mtime: i64,
}

/// Reads the first `len` bytes of the file named by the first argument inside a candidate;
/// `None` when it is missing or unreadable.
pub type ReadMarker<'a> = &'a dyn Fn(&[u8], usize) -> Option<Vec<u8>>;

/// Whether a hit still needs a look inside the entry.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Inside {
    Done,
    /// Call [`RuleSet::check_inside`] when the entry's listing is known.
    Check,
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct Hit {
    /// Index into [`RuleSet::rules`].
    pub rule: u16,
    /// The rule's safety, lowered by a `degrade` condition that holds.
    pub safety: Safety,
    /// The index of that condition in the rule's `degrade`.
    pub degraded: Option<u16>,
    pub inside: Inside,
    /// Some sibling was not found in a partial listing: `safety` and `degraded` assume it is
    /// missing, and [`RuleSet::siblings`] decides once the whole listing is known. A required
    /// sibling that is still missing then drops the hit.
    pub open: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Verdict {
    None,
    /// A protected path: nothing below it is offered for deletion either, unless an exception
    /// lifts it. The state goes to the entry's own entries as their `protection`.
    Protected(protected::State),
    /// Matching rules, best first (priority, then the number of conditions). If the first
    /// needs a look inside and fails it, the next one applies.
    Hits(Vec<Hit>),
}

#[derive(Clone, Debug)]
pub struct RuleSet {
    rules: Vec<Rule>,
    /// Rules matched by name only, with a literal name pattern, by that name (lower case when
    /// names fold case).
    by_name: FastMap<Vec<u8>, Vec<u16>>,
    /// Rules matched by name only, with a wildcard name pattern.
    name_globs: Vec<u16>,
    /// Some rule matched by name only can match a file; otherwise files skip the name rules.
    file_names: bool,
    /// Every path pattern of every rule, with its rule. Rules with names too are tried only in
    /// directories where one of their path patterns can match.
    paths: Vec<(u16, Glob)>,
    protected: Protected,
    fold: bool,
    /// Every sibling name a rule asks about without a variable in it, and the text after the
    /// last variable of those with one (`.ini` for `{name_stem}.ini`).
    sibling_names: FastSet<Vec<u8>>,
    sibling_tails: Vec<Vec<u8>>,
    /// Seconds since the Unix epoch that `not_modified_days` counts back from.
    now: i64,
}

/// What a directory's entries are classified against: the rules and protected paths that can
/// match below `parent`, found once per listing so that each entry costs little.
pub struct Dir<'a> {
    set: &'a RuleSet,
    parent: &'a [u8],
    /// Indices into `RuleSet::paths` whose pattern can match an entry here.
    paths: Vec<usize>,
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

/// The name without its last extension, and without `.tar` before it: `pixel.avd` is
/// `pixel`, `src.tar.gz` is `src`.
pub fn stem(name: &[u8]) -> &[u8] {
    let cut = |n: &'_ [u8]| match n.iter().rposition(|&b| b == b'.') {
        Some(i) if i > 0 => i,
        _ => n.len(),
    };
    let s = &name[..cut(name)];
    let inner = &s[..cut(s)];
    if s.len() < name.len() && s[inner.len()..].eq_ignore_ascii_case(b".tar") {
        inner
    } else {
        s
    }
}

/// `pattern` with `{name}` and `{name_stem}` filled in for the entry named `name`; borrowed
/// when it has neither, as nearly all do.
fn fill<'a>(pattern: &'a str, name: &[u8]) -> Cow<'a, [u8]> {
    if !pattern.contains('{') {
        return Cow::Borrowed(pattern.as_bytes());
    }
    let name_s = String::from_utf8_lossy(name);
    let stem_s = String::from_utf8_lossy(stem(name));
    Cow::Owned(
        pattern
            .replace("{name_stem}", &stem_s)
            .replace("{name}", &name_s)
            .into_bytes(),
    )
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
    pub fn new(rules: Vec<Rule>, env: &Env) -> RuleSet {
        let fold = glob::platform_folds_case(env.platform);
        let mut by_name: FastMap<Vec<u8>, Vec<u16>> = FastMap::default();
        let mut name_globs = Vec::new();
        let mut paths = Vec::new();
        for (i, r) in rules.iter().enumerate() {
            let i = i as u16;
            for g in &r.paths {
                paths.push((i, g.clone()));
            }
            if !r.paths.is_empty() {
                continue;
            }
            for g in &r.names {
                match g.literal() {
                    Some(name) => by_name.entry(name.to_vec()).or_default().push(i),
                    None if !name_globs.contains(&i) => name_globs.push(i),
                    None => {}
                }
            }
        }
        for v in by_name.values_mut() {
            v.dedup();
        }
        let file_names = rules
            .iter()
            .any(|r| r.paths.is_empty() && !r.names.is_empty() && r.kind != Kind::Dir);
        let mut sibling_names = FastSet::default();
        let mut sibling_tails: Vec<Vec<u8>> = Vec::new();
        let asked = rules.iter().flat_map(|r| {
            r.sibling
                .iter()
                .flatten()
                .chain(r.degrade.iter().flat_map(|d| &d.missing_sibling))
        });
        for n in asked {
            match n.rfind('}') {
                Some(i) => {
                    let tail = n.as_bytes()[i + 1..].to_vec();
                    if !sibling_tails.contains(&tail) {
                        sibling_tails.push(tail);
                    }
                }
                None => {
                    sibling_names.insert(n.as_bytes().to_vec());
                }
            }
        }
        RuleSet {
            rules,
            by_name,
            name_globs,
            file_names,
            paths,
            protected: Protected::new(env),
            fold,
            sibling_names,
            sibling_tails,
            now: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| d.as_secs() as i64),
        }
    }

    /// Sets the time `not_modified_days` counts back from (the clock when the set was made).
    pub fn set_now(&mut self, now: i64) {
        self.now = now;
    }

    /// Whether some rule may ask whether an entry named `name` is next to its candidate. A
    /// wide directory keeps only these names for [`RuleSet::siblings`].
    pub fn may_be_sibling(&self, name: &[u8]) -> bool {
        self.sibling_names.contains(name) || self.sibling_tails.iter().any(|t| name.ends_with(t))
    }

    /// The names [`RuleSet::check_inside`] asks about for rule `i` and the entry named `name`:
    /// its `child` names and required marker files.
    pub fn inside_names(&self, i: u16, name: &[u8]) -> Vec<Vec<u8>> {
        let r = self.rule(i);
        let mut v: Vec<Vec<u8>> = Vec::new();
        let names = r.child.iter().flatten().map(|n| fill(n, name).into_owned());
        let markers = r.markers.iter().filter(|m| m.required);
        for n in names.chain(markers.map(|m| m.file.as_bytes().to_vec())) {
            if !v.contains(&n) {
                v.push(n);
            }
        }
        v
    }

    pub fn rules(&self) -> &[Rule] {
        &self.rules
    }

    /// A hash of the rules' ids in order (FNV-1a): two sets with the same fingerprint number
    /// their rules alike, so classes sent between them name the same rules.
    pub fn fingerprint(&self) -> u64 {
        let mut h: u64 = 0xcbf2_9ce4_8422_2325;
        let mut eat = |b: &[u8]| {
            for &x in b {
                h = (h ^ u64::from(x)).wrapping_mul(0x0100_0000_01b3);
            }
        };
        eat(&(self.rules.len() as u64).to_le_bytes());
        for r in &self.rules {
            eat(r.id.as_bytes());
            eat(&[0, r.safety as u8, r.action as u8, r.degrade.len() as u8]);
        }
        h
    }

    pub fn rule(&self, i: u16) -> &Rule {
        &self.rules[i as usize]
    }

    pub fn get(&self, id: &str) -> Option<&Rule> {
        self.rules.iter().find(|r| r.id == id)
    }

    pub fn protected(&self) -> &Protected {
        &self.protected
    }

    /// The context for classifying the entries of the directory at `parent`.
    pub fn dir<'a>(&'a self, parent: &'a [u8]) -> Dir<'a> {
        self.dir_in(parent, None)
    }

    /// [`RuleSet::dir`] trying only the patterns that reach `parent`: those its parent's
    /// listing found ([`Dir::reach`]), or all of them with `None`.
    pub fn dir_in<'a>(&'a self, parent: &'a [u8], reach: Option<&Reach>) -> Dir<'a> {
        let dir = with_slash(parent);
        let depth = depth_below(&dir);
        let candidates: Box<dyn Iterator<Item = u32>> = match reach {
            Some(r) => Box::new(r.paths.iter().copied()),
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
        Dir {
            set: self,
            parent,
            paths,
            protected,
            reach: Reach {
                paths: below,
                protected: reach_protected,
            },
        }
    }

    /// Classifies one entry; for many entries of one directory, [`RuleSet::dir`] once and
    /// [`Dir::classify`] for each is cheaper.
    pub fn classify(&self, c: &Candidate) -> Verdict {
        self.dir(c.parent).classify(c)
    }

    /// The checks after the prefilter, on what the listing shows.
    fn check(&self, i: u16, c: &Candidate, path: &dyn Fn() -> Vec<u8>) -> Option<Hit> {
        let r = self.rule(i);
        if !self.fits(r, c) || (r.top_most && c.inside.contains(&i)) {
            return None;
        }
        if !r.exclude.is_empty() || !r.not_inside.is_empty() {
            let path = path();
            if r.exclude.iter().any(|g| g.matches(&path))
                || r.not_inside
                    .iter()
                    .any(|g| ancestors(&path).any(|a| g.matches(a)))
            {
                return None;
            }
        }
        let siblings = self.siblings(i, c.name, c.sibling);
        let open = c.partial && siblings.is_none_or(|(_, d)| d.is_some());
        if siblings.is_none() && !open {
            return None;
        }
        let (safety, degraded) = siblings.unwrap_or((r.safety, None));
        Some(Hit {
            rule: i,
            safety,
            degraded,
            inside: if r.looks_inside() {
                Inside::Check
            } else {
                Inside::Done
            },
            open,
        })
    }

    /// The checks on the entry's own kind and stat, cheap enough to run before its path is
    /// built.
    fn fits(&self, r: &Rule, c: &Candidate) -> bool {
        let kind_ok = match r.kind {
            Kind::Dir => c.is_dir,
            Kind::File => !c.is_dir,
            Kind::Any => true,
        };
        if !kind_ok {
            return false;
        }
        if r.larger_than == 0 && r.not_modified_days == 0 {
            return true;
        }
        c.stat.is_some_and(|st| {
            let big = r.larger_than == 0 || st.disk > r.larger_than;
            big && self.now - st.mtime >= i64::from(r.not_modified_days) * 86_400
        })
    }

    /// The `sibling` and `degrade` conditions of rule `i` for the entry named `name`, with
    /// `sibling` telling whether the listing has an entry: `None` when a required sibling is
    /// missing, otherwise the safety and the `degrade` condition that holds.
    pub fn siblings(
        &self,
        i: u16,
        name: &[u8],
        sibling: &dyn Fn(&[u8]) -> bool,
    ) -> Option<(Safety, Option<u16>)> {
        let r = self.rule(i);
        let has = |names: &[String]| names.iter().any(|n| sibling(&fill(n, name)));
        if !r.sibling.iter().all(|any_of| has(any_of)) {
            return None;
        }
        let degraded = r
            .degrade
            .iter()
            .position(|d| !d.missing_sibling.is_empty() && !has(&d.missing_sibling));
        Some((
            degraded.map_or(r.safety, |d| r.degrade[d].safety),
            degraded.map(|d| d as u16),
        ))
    }

    /// The `child` and required `marker` conditions of rule `i` for the entry named `name`:
    /// `child` tells whether its listing has an entry, `marker` reads the first bytes of a
    /// file in it.
    pub fn check_inside(
        &self,
        i: u16,
        name: &[u8],
        child: &dyn Fn(&[u8]) -> bool,
        marker: ReadMarker,
    ) -> bool {
        let r = self.rule(i);
        let children_ok = r
            .child
            .iter()
            .all(|any_of| any_of.iter().any(|n| child(&fill(n, name))));
        children_ok
            && r.markers.iter().filter(|m| m.required).all(|m| {
                let want = m.prefix.as_deref().unwrap_or(b"");
                marker(m.file.as_bytes(), want.len()).is_some_and(|got| got.starts_with(want))
            })
    }
}

impl Dir<'_> {
    /// The patterns that can reach this directory's subdirectories, for [`RuleSet::dir_in`].
    pub fn reach(&self) -> &Reach {
        &self.reach
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
        let mut found: Vec<u16> = Vec::new();
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

/// Every proper ancestor path of `path`.
fn ancestors(path: &[u8]) -> impl Iterator<Item = &[u8]> {
    path.iter()
        .enumerate()
        .filter(|&(i, &b)| b == b'/' && i > 0)
        .map(|(i, _)| &path[..i])
}

#[cfg(test)]
mod tests;
