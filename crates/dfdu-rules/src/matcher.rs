//! Matching entries against a rule set, in the evaluation order of
//! <https://dfdu.app/docs/rules/>.
//!
//! [`Dir::classify`] runs for each entry of a directory listing, with the listing at hand for
//! `sibling` checks: the protected-path check, then the name and path prefilter, then
//! `exclude`, `not_inside`, `sibling` and `degrade`. It must stay cheap, since it runs on the
//! applier: a name rule is found by a hash lookup, and the path patterns that can match in a
//! directory are picked once per listing ([`RuleSet::dir`]), so most entries never build their
//! full path. Conditions on what is inside the entry (`child`,
//! required `marker`) are left to [`RuleSet::check_inside`], called once the entry's own
//! listing is known, as is `empty`. A file's own size and age (`larger_than`,
//! `not_modified_days`) are checked here from its stat, and a broken link (`broken_link`) by
//! following the link once its path matched; a rule's `min_size`, `min_age_days` and guards are checked by
//! the cleanup step.

use std::borrow::Cow;
use std::sync::Arc;

use dfdu_tree::{Class, Ix, RuleIx, Safety, Tree};

use crate::hash::{FastMap, FastSet};
use crate::protected::{self, Protected};
use crate::vars::Env;
use crate::{Action, Glob, Kind, Rule, glob};

mod dir;

pub use dir::{Dir, Reach};
pub(crate) use dir::{depth_below, may_match_below};

/// An entry to classify: its directory's path (absolute, `/`-separated), its name, and the
/// rest of the listing.
pub struct Candidate<'a> {
    pub parent: &'a [u8],
    pub name: &'a [u8],
    pub is_dir: bool,
    /// True when the listing of `parent` has an entry with this name.
    pub sibling: &'a dyn Fn(&[u8]) -> bool,
    /// Rules matched by the directories above, innermost last.
    pub inside: &'a [RuleIx],
    /// Whether the directory is protected (from its own [`Verdict::Protected`]).
    pub protection: protected::State,
    /// `sibling` knows only part of the listing (a wide directory read in several blocks), so
    /// a sibling it does not find may still be there: such a hit is marked [`Hit::open`].
    pub partial: bool,
    /// A file's stat, for `larger_than` and `not_modified_days`; `None` for a directory or
    /// when not known, which fails those conditions.
    pub stat: Option<Stat>,
    /// A symbolic link: tells whether its target is missing, for `broken_link`. `None` for
    /// anything else, or when that cannot be found out, which fails that condition.
    pub link: Option<&'a dyn Fn() -> bool>,
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
    pub rule: RuleIx,
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

/// The most rules a [`RuleSet`] holds: one per [`RuleIx`] value.
pub const MAX_RULES: usize = u16::MAX as usize + 1;

#[derive(Clone, Debug)]
pub struct RuleSet {
    rules: Vec<Rule>,
    /// Rules matched by name only, with a literal name pattern, by that name (lower case when
    /// names fold case).
    by_name: FastMap<Vec<u8>, Vec<RuleIx>>,
    /// Rules matched by name only, with a wildcard name pattern.
    name_globs: Vec<RuleIx>,
    /// Some rule matched by name only can match a file; otherwise files skip the name rules.
    file_names: bool,
    /// Every path pattern of every rule, with its rule. Rules with names too are tried only in
    /// directories where one of their path patterns can match.
    paths: Vec<(RuleIx, Glob)>,
    protected: Arc<Protected>,
    fold: bool,
    /// Every sibling name a rule asks about without a variable in it, and the text after the
    /// last variable of those with one (`.ini` for `{name_stem}.ini`), with the rule asking.
    sibling_names: FastSet<Vec<u8>>,
    sibling_tails: Vec<(Vec<u8>, RuleIx)>,
    /// Seconds since the Unix epoch that `not_modified_days` counts back from.
    now: i64,
}

/// The name without its last extension, and without `.tar` before it: `pixel.avd` is
/// `pixel`, `src.tar.gz` is `src`.
#[must_use]
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

impl RuleSet {
    /// Compiles `rules` for `env`; a rule's [`RuleIx`] is its position in `rules`.
    ///
    /// # Panics
    ///
    /// There are more than [`MAX_RULES`] rules ([`load_from`](crate::load_from) keeps the
    /// first [`MAX_RULES`] and reports the rest).
    #[must_use]
    pub fn new(rules: Vec<Rule>, env: &Env) -> RuleSet {
        assert!(
            rules.len() <= MAX_RULES,
            "{} rules, at most {MAX_RULES} fit a rule index",
            rules.len()
        );
        let fold = glob::platform_folds_case(env.platform);
        let mut by_name: FastMap<Vec<u8>, Vec<RuleIx>> = FastMap::default();
        let mut name_globs = Vec::new();
        let mut paths = Vec::new();
        for (i, r) in (0..=u16::MAX).zip(&rules) {
            let i = RuleIx(i);
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
        let mut sibling_tails: Vec<(Vec<u8>, RuleIx)> = Vec::new();
        for (i, r) in (0..=u16::MAX).zip(&rules) {
            let asked = r
                .sibling
                .iter()
                .flatten()
                .chain(r.degrade.iter().flat_map(|d| &d.missing_sibling));
            for n in asked {
                match n.rfind('}') {
                    Some(k) => {
                        let tail = (n.as_bytes()[k + 1..].to_vec(), RuleIx(i));
                        if !sibling_tails.contains(&tail) {
                            sibling_tails.push(tail);
                        }
                    }
                    None => {
                        sibling_names.insert(n.as_bytes().to_vec());
                    }
                }
            }
        }
        RuleSet {
            rules,
            by_name,
            name_globs,
            file_names,
            paths,
            protected: Arc::new(Protected::new(env)),
            fold,
            sibling_names,
            sibling_tails,
            now: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX)),
        }
    }

    /// Sets the time `not_modified_days` counts back from (the clock when the set was made).
    pub fn set_now(&mut self, now: i64) {
        self.now = now;
    }

    /// The names [`RuleSet::check_inside`] asks about for rule `i` and the entry named `name`:
    /// its `child` names and required marker files.
    #[must_use]
    pub fn inside_names(&self, i: RuleIx, name: &[u8]) -> Vec<Vec<u8>> {
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

    #[must_use]
    pub fn rules(&self) -> &[Rule] {
        &self.rules
    }

    /// A hash of the rules in order (FNV-1a): their ids, safety, action, degrade conditions
    /// and compiled path patterns, as variables and links expanded them. Two sets with the
    /// same fingerprint number their rules alike, so classes sent between them name the same
    /// rules, and match the same paths, so a class one decides the other decides alike.
    #[must_use]
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
            let action = match r.action {
                Action::Delete => 0,
                Action::Trash => 1,
                Action::Native(_) => 2,
                Action::Report => 3,
            };
            #[expect(
                clippy::cast_possible_truncation,
                reason = "hashed: the length wraps alike on every side that compares fingerprints"
            )]
            eat(&[0, r.safety as u8, action, r.degrade.len() as u8]);
            for list in [&r.paths, &r.exclude, &r.not_inside] {
                eat(&(list.len() as u64).to_le_bytes());
                for g in list {
                    eat(g.pattern());
                    eat(&[0]);
                }
            }
        }
        h
    }

    #[must_use]
    pub fn rule(&self, i: RuleIx) -> &Rule {
        &self.rules[usize::from(i.0)]
    }

    #[must_use]
    pub fn get(&self, id: &str) -> Option<&Rule> {
        self.rules.iter().find(|r| r.id == id)
    }

    /// Whether rule `i` stops the descent: its matches cover everything below them.
    #[must_use]
    pub fn stops_descent(&self, i: RuleIx) -> bool {
        self.rules
            .get(usize::from(i.0))
            .is_some_and(|r| r.stop_descent)
    }

    /// The class that applies to node `ix` of `tree`, and the node it is on: the nearest
    /// match, unless a match above it stops the descent ([`Tree::class_in_effect`]). The
    /// browser's rows, panes and treemap, the cleanup listing and the reports all ask this.
    #[must_use]
    pub fn class_in_effect(&self, tree: &Tree, ix: Ix) -> Option<(Ix, Class)> {
        tree.class_in_effect(ix, |c| self.stops_descent(c.rule))
    }

    /// [`RuleSet::class_in_effect`] at `ix` given `above`, the class in effect at its parent
    /// ([`Tree::class_below`]).
    #[must_use]
    pub fn class_below(
        &self,
        tree: &Tree,
        above: Option<(Ix, Class)>,
        ix: Ix,
    ) -> Option<(Ix, Class)> {
        tree.class_below(above, ix, |c| self.stops_descent(c.rule))
    }

    /// The index of the rule with this id.
    #[must_use]
    pub fn index(&self, id: &str) -> Option<RuleIx> {
        let i = self.rules.iter().position(|r| r.id == id)?;
        u16::try_from(i).ok().map(RuleIx)
    }

    /// Paths compare ignoring ASCII case (macOS, Windows); patterns are in lower case then.
    #[must_use]
    pub fn folds_case(&self) -> bool {
        self.fold
    }

    /// The protected paths, for the environment the set was compiled for.
    #[must_use]
    pub fn protected(&self) -> &Arc<Protected> {
        &self.protected
    }

    /// Classifies one entry; for many entries of one directory, [`RuleSet::dir`] once and
    /// [`Dir::classify`] for each is cheaper.
    #[must_use]
    pub fn classify(&self, c: &Candidate) -> Verdict {
        self.dir(c.parent).classify(c)
    }

    /// The checks after the prefilter, on what the listing shows.
    fn check(&self, i: RuleIx, c: &Candidate, path: &dyn Fn() -> Vec<u8>) -> Option<Hit> {
        let r = self.rule(i);
        if !self.fits(r, c) || (r.top_most && c.inside.contains(&i)) {
            return None;
        }
        if r.broken_link && !c.link.is_some_and(|missing| missing()) {
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
        i: RuleIx,
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
            degraded.and_then(|d| u16::try_from(d).ok()),
        ))
    }

    /// The `child`, required `marker` and `empty` conditions of rule `i` for the entry named
    /// `name`: `child` tells whether its listing has an entry, `marker` reads the first bytes
    /// of a file in it, `empty` whether the listing had no entries at all.
    pub fn check_inside(
        &self,
        i: RuleIx,
        name: &[u8],
        child: &dyn Fn(&[u8]) -> bool,
        marker: ReadMarker,
        empty: bool,
    ) -> bool {
        let r = self.rule(i);
        if r.empty && !empty {
            return false;
        }
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

/// Every proper ancestor path of `path`.
pub(crate) fn ancestors(path: &[u8]) -> impl Iterator<Item = &[u8]> {
    path.iter()
        .enumerate()
        .filter(|&(i, &b)| b == b'/' && i > 0)
        .map(|(i, _)| &path[..i])
}

#[cfg(test)]
mod tests;
