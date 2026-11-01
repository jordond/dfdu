//! Files with identical contents below the scan root, found with [`dfdu_dupes`] in a thread of
//! their own ([`Run`]), and the rules for which copy of a group is kept ([`Keep`], [`kept`]) and
//! which are the copies to delete ([`copies`], [`suggested`]).
//!
//! A file of the result is looked up in the tree by path once; `nodes` maps each to its node,
//! `None` once it is gone from the tree (deleted). A group needs two files still in the tree.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, OnceLock};

use dfdu_dupes::{Candidate, Found, Group, Options, Progress};
use dfdu_tree::NodeRef;

/// The node of every file of the result looked up so far, `None` for one gone from the tree.
pub type Nodes = HashMap<PathBuf, Option<NodeRef>>;

/// Which copy of a group marking leaves unmarked. Ties go to the first in path order.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub enum Keep {
    #[default]
    Oldest,
    Newest,
    ShortestPath,
    FirstPath,
}

impl Keep {
    /// Every rule, in the order `K` steps through them.
    pub const ALL: [Keep; 4] = [
        Keep::Oldest,
        Keep::Newest,
        Keep::ShortestPath,
        Keep::FirstPath,
    ];

    /// The rule after this one in [`Keep::ALL`], round to the first.
    #[must_use]
    pub fn next(self) -> Keep {
        match self {
            Keep::Oldest => Keep::Newest,
            Keep::Newest => Keep::ShortestPath,
            Keep::ShortestPath => Keep::FirstPath,
            Keep::FirstPath => Keep::Oldest,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Keep::Oldest => "oldest",
            Keep::Newest => "newest",
            Keep::ShortestPath => "shortest path",
            Keep::FirstPath => "first in path order",
        }
    }
}

/// The node of `c` when it is still in the tree.
pub fn live(nodes: &Nodes, c: &Candidate) -> Option<NodeRef> {
    nodes.get(&c.path).copied().flatten()
}

/// The index in `group` of the file still in the tree that `keep` keeps.
pub fn kept(group: &Group, keep: Keep, nodes: &Nodes) -> Option<usize> {
    let left = (group.files.iter().enumerate()).filter(|(_, c)| live(nodes, c).is_some());
    // Compared by reference: this runs for every group on every frame.
    let order = |a: &Candidate, b: &Candidate| match keep {
        Keep::Oldest => a.mtime.cmp(&b.mtime),
        Keep::Newest => b.mtime.cmp(&a.mtime),
        Keep::ShortestPath => (a.path.as_os_str().len()).cmp(&b.path.as_os_str().len()),
        Keep::FirstPath => std::cmp::Ordering::Equal,
    };
    left.min_by(|(_, a), (_, b)| order(a, b).then_with(|| a.path.cmp(&b.path)))
        .map(|(i, _)| i)
}

/// The files of `group` still in the tree, but the one [`kept`].
pub fn copies(group: &Group, keep: Keep, nodes: &Nodes) -> Vec<(Candidate, NodeRef)> {
    let kept = kept(group, keep, nodes);
    (group.files.iter().enumerate())
        .filter(|&(i, _)| Some(i) != kept)
        .filter_map(|(_, c)| Some((c.clone(), live(nodes, c)?)))
        .collect()
}

/// What marking every group suggests deleting: the [`copies`] of each group that frees
/// something. Groups of clones share their data, so deleting a copy frees nothing.
pub fn suggested(found: &Found, keep: Keep, nodes: &Nodes) -> Vec<(Candidate, NodeRef)> {
    (found.groups.iter())
        .filter(|g| !g.all_clones())
        .flat_map(|g| copies(g, keep, nodes))
        .collect()
}

/// A comparison, running or done.
#[derive(Clone, Debug)]
pub struct Run {
    /// The scan root the files are below.
    pub root: PathBuf,
    pub progress: Arc<Progress>,
    /// Set to stop the comparison.
    pub cancel: Arc<AtomicBool>,
    /// What it found, once it is done.
    pub result: Arc<OnceLock<Found>>,
}

impl Run {
    /// Starts comparing `cands` in a thread, with as many threads reading as the storage under
    /// `root` serves well.
    ///
    /// # Errors
    ///
    /// When the thread cannot start.
    pub fn start(root: PathBuf, cands: Vec<Candidate>) -> std::io::Result<Run> {
        let run = Run {
            root,
            progress: Arc::default(),
            cancel: Arc::default(),
            result: Arc::default(),
        };
        let (progress, cancel, result) =
            (run.progress.clone(), run.cancel.clone(), run.result.clone());
        let root_path = run.root.clone();
        std::thread::Builder::new()
            .name("dfdu-dupes".into())
            .spawn(move || {
                let opts = Options::for_storage(dfdu_sys::storage(&root_path));
                let _ = result.set(dfdu_dupes::find(&cands, &opts, &progress, &cancel));
            })?;
        Ok(run)
    }
}

#[cfg(test)]
mod tests;
