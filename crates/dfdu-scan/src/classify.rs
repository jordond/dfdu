//! Classification on the scan workers.
//!
//! The worker that stats a block classifies its entries against the job's rules before it sends
//! the chunk, so categories appear during the scan and the cost is spread over the workers. What
//! a directory's entries are classified against travels with the directory's task
//! ([`TaskClass`]): its path, the rules decided above it and whether it is protected.
//!
//! A hit is decided in one of three places:
//! - **In the parent's block,** when it needs nothing more: the class goes in the chunk.
//! - **In the entry's own listing,** when a rule looks inside (`child`, a required marker such
//!   as CACHEDIR.TAG): the hits go with the subdirectory's task, and the worker that reads the
//!   subdirectory decides them, in its block when the listing is one block, or when the last
//!   part of the listing ends. The class goes in the subdirectory's `Close`.
//! - **When a wide parent's listing ends,** when a sibling may be in another block of the
//!   parent: the parent keeps the names rules may ask
//!   about, and its last part checks the siblings. Such a subdirectory needs both its own
//!   listing and its parent's, which end in either order; whichever ends second decides, under
//!   the [`Rendezvous`] lock, and sends the class in its `Close`. Files in a wide directory do
//!   not wait: a sibling not in their block counts as missing, which can only lower a match.
//!
//! The directories below a match are classified with the rules decided above them, so a
//! `stop_descent` rule skips them. A rule decided only when a listing ends comes too late for
//! the tasks of that listing's subdirectories; what they match then is covered by the match
//! above them, which [`dfdu_tree::Tree::class_in_effect`] picks.

use std::cell::OnceCell;
use std::sync::Arc;

use dfdu_rules::hash::FastSet as HashSet;
use dfdu_rules::protected::State;
use dfdu_rules::{
    Candidate, Hit, Inside, Reach, RuleIx, RuleSet, Safety, Stat, Verdict, rule_path,
};
use dfdu_tree::{Class, Kind};

use crate::proto::{DirId, Entry, FileRec, Mark, NameSpan, SubdirRec};
use crate::sync::Mutex;

/// What a job classifies against ([`crate::JobOptions::class`]).
#[derive(Clone, Debug)]
pub struct ClassOpts {
    pub rules: Arc<RuleSet>,
    /// The job root's absolute path, as rules see it ([`rule_path`] of the canonical path).
    pub path: Vec<u8>,
    /// Rules decided for the directories above the job root, innermost last.
    pub inside: Vec<RuleIx>,
    /// Whether the job root is protected.
    pub protection: State,
    /// The job root's own hits, from its parent's listing, to check against its own.
    pub chain: Vec<Hit>,
}

/// A rule's sibling conditions as decided on a whole listing ([`RuleSet::siblings`]).
type Sib = Option<(Safety, Option<u16>)>;

/// What a directory's task carries for classification.
pub struct TaskClass {
    /// Absolute, `/`-separated, as rules are written.
    path: Vec<u8>,
    /// Where the directory's own name starts in `path`.
    name_at: usize,
    /// Rules decided for the directories above, innermost last.
    inside: Vec<RuleIx>,
    protection: State,
    /// The directory's own rule, when its parent decided it.
    decided: Option<RuleIx>,
    /// Hits to decide on the directory's own listing, best first; empty when decided.
    chain: Vec<Hit>,
    /// Set when a hit in `chain` waits for the parent's whole listing.
    rv: Option<Arc<Rendezvous>>,
    /// The path patterns that can reach this directory, found by its parent's listing; `None`
    /// tries them all.
    reach: Option<Arc<Reach>>,
}

impl ClassOpts {
    /// What a scan of `root` classifies against: nothing decided above it, and its own hits
    /// from its parent directory, whose entries it asks about one by one (a few names, such as
    /// `Cargo.toml` next to a `target` root).
    #[must_use]
    pub fn for_root(rules: Arc<RuleSet>, root: &std::path::Path) -> ClassOpts {
        let canon = dunce::canonicalize(root).unwrap_or_else(|_| root.to_path_buf());
        let path = rule_path(&canon);
        let mut chain = Vec::new();
        let mut protection = rules.protected().state_of(&path);
        if let (Some(parent), Some(name)) = (canon.parent(), canon.file_name()) {
            let name = dfdu_tree::name_bytes(name);
            let sibling =
                |n: &[u8]| std::fs::symlink_metadata(parent.join(dfdu_tree::name_os(n))).is_ok();
            let parent_path = rule_path(parent);
            let verdict = rules.classify(&Candidate {
                parent: &parent_path,
                name: &name,
                is_dir: true,
                sibling: &sibling,
                inside: &[],
                protection: rules.protected().state_of(&parent_path),
                partial: false,
                stat: None,
                link: None,
            });
            match verdict {
                Verdict::Hits(h) => chain = h,
                Verdict::Protected(s) => protection = s,
                Verdict::None => {}
            }
        }
        ClassOpts {
            rules,
            path,
            inside: Vec::new(),
            protection,
            chain,
        }
    }
}

impl TaskClass {
    #[must_use]
    pub fn root(opts: &ClassOpts) -> TaskClass {
        let path = opts.path.clone();
        let name_at = path.iter().rposition(|&b| b == b'/').map_or(0, |i| i + 1);
        TaskClass {
            path,
            name_at,
            inside: opts.inside.clone(),
            protection: opts.protection,
            decided: None,
            chain: opts.chain.clone(),
            rv: None,
            reach: None,
        }
    }
}

/// The meeting point of a subdirectory's own listing and its wide parent's: each side leaves
/// what it found, and the side that comes second decides.
#[derive(Default)]
pub struct Rendezvous(Mutex<Sides>);

/// What each side of a [`Rendezvous`] left: the subdirectory's looks inside, the parent's
/// siblings.
type Sides = (Option<Vec<bool>>, Option<Vec<Sib>>);

impl Rendezvous {
    /// The subdirectory's side: whether each hit's look inside held. `Some` with the class when
    /// the parent's side is already there.
    #[expect(
        clippy::option_option,
        reason = "`None`: the other side is not there yet; `Some(None)`: it is, and there is no class"
    )]
    fn inside(&self, chain: &[Hit], inside: Vec<bool>) -> Option<Option<Class>> {
        let mut g = self.0.lock();
        if let Some(sib) = &g.1 {
            Some(pick(chain, &inside, Some(sib)))
        } else {
            g.0 = Some(inside);
            None
        }
    }

    /// The parent's side: each hit's sibling conditions.
    #[expect(
        clippy::option_option,
        reason = "`None`: the other side is not there yet; `Some(None)`: it is, and there is no class"
    )]
    fn siblings(&self, chain: &[Hit], sib: Vec<Sib>) -> Option<Option<Class>> {
        let mut g = self.0.lock();
        if let Some(inside) = &g.0 {
            Some(pick(chain, inside, Some(&sib)))
        } else {
            g.1 = Some(sib);
            None
        }
    }
}

/// The first hit of `chain` that holds: its look inside held (`inside`), and its siblings, for
/// an open hit, held on the whole listing (`sib`; without it, as found in its block).
fn pick(chain: &[Hit], inside: &[bool], sib: Option<&[Sib]>) -> Option<Class> {
    chain.iter().enumerate().find_map(|(k, h)| {
        let (safety, degraded) = match sib {
            Some(s) if h.open => s[k]?,
            _ => (h.safety, h.degraded),
        };
        (h.inside == Inside::Done || inside[k]).then_some(Class {
            rule: h.rule,
            safety,
            degraded,
            flags: 0,
        })
    })
}

fn class_of(h: &Hit) -> Class {
    Class {
        rule: h.rule,
        safety: h.safety,
        degraded: h.degraded,
        flags: 0,
    }
}

/// A subdirectory whose hits wait for its parent's whole listing.
struct OpenEntry {
    dir: DirId,
    name: Vec<u8>,
    chain: Vec<Hit>,
    rv: Arc<Rendezvous>,
}

#[derive(Default)]
struct ListState {
    /// For each of `Listing::wanted`, whether the listing has it, and the first bytes of a
    /// marker file, read when its block was classified.
    seen: Vec<bool>,
    marker: Vec<Option<Vec<u8>>>,
    /// The chain was decided.
    decided: bool,
    /// Some block had an entry.
    entries: bool,
    /// A wide listing's names some rule may ask about as a sibling.
    siblings: HashSet<Vec<u8>>,
    open: Vec<OpenEntry>,
    /// Classes to send with the `Close`.
    close: Vec<(DirId, Class)>,
}

/// Classification state of one directory listing, shared by its listing task and the chunk
/// tasks stat'ing its blocks.
pub struct Listing {
    rules: Arc<RuleSet>,
    dir: DirId,
    task: TaskClass,
    /// The names the chain's looks inside ask about, with how many bytes to read of each
    /// that is a marker file.
    wanted: Vec<(Vec<u8>, Option<usize>)>,
    state: Mutex<ListState>,
}

/// Reads the first bytes of a file in the directory being listed (`Backend::read_prefix`).
pub type ReadPrefix<'a> = &'a dyn Fn(&[u8], usize) -> Option<Vec<u8>>;

/// Whether a symbolic link in the directory being listed points at nothing
/// (`Backend::link_target_missing`).
pub type TargetMissing<'a> = &'a dyn Fn(&[u8]) -> bool;

/// What classifying a block produced.
#[derive(Default)]
pub struct BlockClass {
    pub marks: Vec<(Entry, Mark)>,
    /// One per subdirectory of the block, in order.
    pub subdirs: Vec<Option<Box<TaskClass>>>,
}

impl Listing {
    #[must_use]
    pub fn new(rules: Arc<RuleSet>, dir: DirId, task: TaskClass) -> Listing {
        let name = &task.path[task.name_at..];
        let mut wanted: Vec<(Vec<u8>, Option<usize>)> = Vec::new();
        for h in task.chain.iter().filter(|h| h.inside == Inside::Check) {
            let r = rules.rule(h.rule);
            for n in rules.inside_names(h.rule, name) {
                let len = r
                    .markers
                    .iter()
                    .filter(|m| m.required && m.file.as_bytes() == n)
                    .map(|m| m.prefix.as_ref().map_or(0, Vec::len))
                    .max();
                match wanted.iter_mut().find(|w| w.0 == n) {
                    Some(w) => w.1 = w.1.max(len),
                    None => wanted.push((n, len)),
                }
            }
        }
        let state = ListState {
            seen: vec![false; wanted.len()],
            marker: vec![None; wanted.len()],
            decided: task.chain.is_empty(),
            ..ListState::default()
        };
        Listing {
            rules,
            dir,
            task,
            wanted,
            state: Mutex::new(state),
        }
    }

    /// Classifies one block. `whole` is set when the block is the directory's whole listing;
    /// `first_subdir` is the id of the block's first subdirectory.
    #[expect(
        clippy::too_many_arguments,
        reason = "the parts of a block as the worker holds them, borrowed for the call"
    )]
    #[expect(
        clippy::too_many_lines,
        reason = "files, then subdirectories, against one locked state and one set of closures"
    )]
    #[expect(
        clippy::cast_possible_truncation,
        reason = "entry indexes within a block, which holds at most `Limits::block_entries`"
    )]
    pub fn block(
        &self,
        names: &[u8],
        files: &[FileRec],
        subdirs: &[SubdirRec],
        others: &[NameSpan],
        first_subdir: DirId,
        whole: bool,
        read: ReadPrefix,
        missing: TargetMissing,
    ) -> BlockClass {
        let name = |s: NameSpan| &names[s.off as usize..s.off as usize + s.len as usize];
        let all = || {
            files
                .iter()
                .map(|f| f.name)
                .chain(subdirs.iter().map(|s| s.name))
                .chain(others.iter().copied())
        };
        let set: OnceCell<HashSet<&[u8]>> = OnceCell::new();
        let in_block = |n: &[u8]| set.get_or_init(|| all().map(name).collect()).contains(n);
        let mut st = self.state.lock();
        st.entries |= all().next().is_some();
        for s in all() {
            let n = name(s);
            if let Some(k) = self.wanted.iter().position(|w| w.0[..] == *n) {
                st.seen[k] = true;
                // Only a regular file is read: a FIFO could block the worker.
                let file = files
                    .iter()
                    .any(|f| f.name == s && f.meta.kind == Kind::File);
                if let (Some(len), true) = (self.wanted[k].1, file) {
                    st.marker[k] = read(n, len);
                }
            }
            if !whole && self.rules.may_be_sibling(n) {
                st.siblings.insert(n.to_vec());
            }
        }
        if whole && !st.decided {
            self.decide(&mut st);
        }
        let mut inside = self.task.inside.clone();
        inside.extend(self.task.decided);
        if whole && let Some((_, c)) = st.close.iter().find(|(d, _)| *d == self.dir) {
            inside.push(c.rule);
        }
        let dir = self
            .rules
            .dir_in(&self.task.path, self.task.reach.as_deref());
        let reach = (!subdirs.is_empty()).then(|| Arc::new(dir.reach().clone()));
        let mut out = BlockClass::default();
        let parent_protected = self.task.protection.protected();
        let classify =
            |n: &[u8], is_dir: bool, stat: Option<Stat>, link: Option<&dyn Fn() -> bool>| {
                dir.classify(&Candidate {
                    parent: &self.task.path,
                    name: n,
                    is_dir,
                    sibling: &in_block,
                    inside: &inside,
                    protection: self.task.protection,
                    partial: !whole,
                    stat,
                    link,
                })
            };
        for (i, f) in files.iter().enumerate() {
            let n = name(f.name);
            let entry = Entry::File(i as u32);
            let stat = Stat {
                disk: f.meta.alloc,
                mtime: f.meta.mtime,
            };
            let target_missing = || missing(n);
            let link =
                (f.meta.kind == Kind::Symlink).then_some(&target_missing as &dyn Fn() -> bool);
            match classify(n, false, Some(stat), link) {
                Verdict::None if parent_protected => out.marks.push((entry, Mark::Excepted)),
                Verdict::Protected(_) if !parent_protected => {
                    out.marks.push((entry, Mark::Protected));
                }
                Verdict::None | Verdict::Protected(_) => {}
                Verdict::Hits(chain) => {
                    if parent_protected {
                        out.marks.push((entry, Mark::Excepted));
                    }
                    // Nothing to look inside a file, and a sibling not in the block is missing.
                    let sib: Vec<Sib> = chain
                        .iter()
                        .map(|h| self.rules.siblings(h.rule, n, &in_block))
                        .collect();
                    let none = vec![false; chain.len()];
                    if let Some(c) = pick(&chain, &none, Some(&sib)) {
                        out.marks.push((entry, Mark::Class(c)));
                    }
                }
            }
        }
        for (i, s) in subdirs.iter().enumerate() {
            let n = name(s.name);
            let entry = Entry::Subdir(i as u32);
            let path = child_path(&self.task.path, n);
            let mut task = TaskClass {
                name_at: path.len() - n.len(),
                path,
                inside: inside.clone(),
                protection: State::Off,
                decided: None,
                chain: Vec::new(),
                rv: None,
                reach: reach.clone(),
            };
            match classify(n, true, None, None) {
                Verdict::None if parent_protected => out.marks.push((entry, Mark::Excepted)),
                Verdict::None => {}
                Verdict::Protected(state) => {
                    if !parent_protected {
                        out.marks.push((entry, Mark::Protected));
                    }
                    task.protection = state;
                }
                Verdict::Hits(mut chain) => {
                    if parent_protected {
                        out.marks.push((entry, Mark::Excepted));
                    }
                    // Hits after one that needs nothing more never apply.
                    if let Some(k) = chain
                        .iter()
                        .position(|h| h.inside == Inside::Done && !h.open)
                    {
                        chain.truncate(k + 1);
                    }
                    if chain.len() == 1 && chain[0].inside == Inside::Done && !chain[0].open {
                        out.marks.push((entry, Mark::Class(class_of(&chain[0]))));
                        task.decided = Some(chain[0].rule);
                    } else {
                        if chain.iter().any(|h| h.open) {
                            let rv = Arc::new(Rendezvous::default());
                            st.open.push(OpenEntry {
                                dir: DirId(first_subdir.0 + i as u32),
                                name: n.to_vec(),
                                chain: chain.clone(),
                                rv: rv.clone(),
                            });
                            task.rv = Some(rv);
                        }
                        task.chain = chain;
                    }
                }
            }
            out.subdirs.push(Some(Box::new(task)));
        }
        out
    }

    /// Decides the directory's own chain from what its listing had.
    fn decide(&self, st: &mut ListState) {
        st.decided = true;
        let name = &self.task.path[self.task.name_at..];
        let inside: Vec<bool> = self
            .task
            .chain
            .iter()
            .map(|h| {
                h.inside == Inside::Check && {
                    let at = |n: &[u8]| self.wanted.iter().position(|w| w.0[..] == *n);
                    let child = |n: &[u8]| at(n).is_some_and(|k| st.seen[k]);
                    let marker = |f: &[u8], _: usize| st.marker[at(f)?].clone();
                    self.rules
                        .check_inside(h.rule, name, &child, &marker, !st.entries)
                }
            })
            .collect();
        let class = match &self.task.rv {
            Some(rv) => rv.inside(&self.task.chain, inside).flatten(),
            None => pick(&self.task.chain, &inside, None),
        };
        if let Some(c) = class {
            st.close.push((self.dir, c));
        }
    }

    /// Called by the part that sends the directory's `Close`, once every block was classified:
    /// decides what waited for the whole listing and returns the classes for the `Close`. Reads
    /// nothing, so it may run under the outbox lock.
    pub fn finish(&self) -> Vec<(DirId, Class)> {
        let mut st = self.state.lock();
        if !st.decided {
            self.decide(&mut st);
        }
        let open = std::mem::take(&mut st.open);
        let names = std::mem::take(&mut st.siblings);
        let has = |n: &[u8]| names.contains(n);
        for e in open {
            let sib: Vec<Sib> = e
                .chain
                .iter()
                .map(|h| self.rules.siblings(h.rule, &e.name, &has))
                .collect();
            if let Some(Some(c)) = e.rv.siblings(&e.chain, sib) {
                st.close.push((e.dir, c));
            }
        }
        std::mem::take(&mut st.close)
    }
}

/// The rule path of an entry named `name` in the directory at `parent`.
fn child_path(parent: &[u8], name: &[u8]) -> Vec<u8> {
    let mut p = Vec::with_capacity(parent.len() + 1 + name.len());
    p.extend_from_slice(parent);
    if !p.ends_with(b"/") {
        p.push(b'/');
    }
    p.extend_from_slice(name);
    p
}
