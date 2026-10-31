//! The Duplicates view (`4`): files with identical contents below the scan root, found on
//! request with [`dfdu_dupes`] once the scan is done; one of the review views
//! (`features::review`).
//!
//! Switching the view on compares nothing: it says how many files a comparison would read
//! ([`count`]), and `Enter` (or `R`) starts one in a thread of its own, from the candidates read
//! under the read lock ([`Effect::FindDupes`]); the view shows its progress and then the groups,
//! the most space to free first, clones last and closed. Ticks are the list's marks: `Space`
//! ticks a copy, or on a group every copy but the one kept, `A` does that for every group, and
//! `d` and `D` delete what is ticked through the usual dialog, which checks each file against
//! the scan. The copy kept is the oldest, and `K` cycles the rule ([`Keep`]); `R` compares
//! again. Groups of APFS clones, which free nothing, are listed but `A` leaves them alone.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock};

use dfdu_dupes::{Candidate, Found, Group, Options, Progress};
use dfdu_engine::Model;
use dfdu_tree::{HostPath, NodeRef};
use ratatui::crossterm::event::{KeyCode, KeyEvent};

use crate::app::keys::{self, Action};
use crate::app::views::{self, View};
use crate::app::{App, Effect};
use crate::state::marks;

mod draw;
mod rows;

pub use draw::draw;
use rows::{Row, copies, kept, live, mark_all, mark_of, open, rows};

/// Files smaller than this are not compared: the size `dfdu dupes` starts from.
pub fn min_size() -> u64 {
    Options::default().min_size
}

/// What the view keeps between frames.
#[derive(Clone, Debug, Default)]
pub struct Dupes {
    cursor: usize,
    scroll: usize,
    run: Option<Run>,
    keep: Keep,
    /// Groups opened or closed against their default (open, but clones closed), by index in
    /// the result; cleared by a new comparison.
    toggled: HashSet<usize>,
    /// How many files a comparison would read, counted while the view waits to be asked, and
    /// the tree's version they were counted at.
    candidates: Option<usize>,
    counted_at: Option<u64>,
    /// The groups with two files left, counted when the files' nodes were last read; `None`
    /// while the view is closed.
    live_groups: Option<usize>,
}

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

    fn next(self) -> Keep {
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

/// The next keep rule, with the status line saying so. A copy the new rule keeps is unticked:
/// the copy kept is never one that `d` deletes.
pub fn cycle_keep(app: &mut App) {
    let d = &mut app.dupes;
    d.keep = d.keep.next();
    let kept_now: Vec<(Candidate, NodeRef)> = found(app)
        .map(|f| {
            (f.groups.iter())
                .filter_map(|g| {
                    let c = &g.files[kept(app, g)?];
                    live(app, c).map(|n| (c.clone(), n))
                })
                .filter(|(_, n)| app.marks.contains(*n))
                .collect()
        })
        .unwrap_or_default();
    for (c, n) in &kept_now {
        marks::set(app, mark_of(c, *n), false);
    }
    app.status = Some(format!(
        "duplicates: keep the {} copy ({} ticks the others)",
        app.dupes.keep.label(),
        crate::app::keys::first_key(crate::app::keys::Action::TickSuggested)
    ));
}

/// The nodes of the files the view lists (still in the tree), for `d` to act on its ticks
/// only; `None` before a comparison finished.
pub fn listed(app: &App) -> Option<HashSet<NodeRef>> {
    let f = found(app)?;
    Some(
        (f.groups.iter())
            .flat_map(|g| &g.files)
            .filter_map(|c| live(app, c))
            .collect(),
    )
}

/// A group whose every copy still in the tree is ticked, by its file's name: deleting what is
/// ticked would leave no copy of it.
pub fn whole_group_ticked(app: &App) -> Option<String> {
    let f = found(app)?;
    f.groups.iter().find_map(|g| {
        let left: Vec<NodeRef> = g.files.iter().filter_map(|c| live(app, c)).collect();
        let all = left.len() >= 2 && left.iter().all(|&n| app.marks.contains(n));
        all.then(|| {
            let first = g.files.iter().find(|c| live(app, c).is_some())?;
            Some(first.path.file_name()?.to_string_lossy().into_owned())
        })
        .flatten()
    })
}

/// A comparison, running or done.
#[derive(Clone, Debug)]
struct Run {
    root: PathBuf,
    progress: Arc<Progress>,
    cancel: Arc<AtomicBool>,
    result: Arc<OnceLock<Found>>,
}

/// True when the view is shown, and so takes the keys it knows. Nothing is compared until the
/// user asks (`Enter` or `R`): before that the view says how many files a comparison would read.
pub fn active(app: &App) -> bool {
    views::current(app) == View::Duplicates
}

/// A comparison is running: the view's progress line needs frames.
pub fn running(app: &App) -> bool {
    active(app)
        && app
            .dupes
            .run
            .as_ref()
            .is_some_and(|r| r.result.get().is_none())
}

fn found(app: &App) -> Option<&Found> {
    app.dupes.run.as_ref()?.result.get()
}

/// Where a comparison is, for the view's tab.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Groups {
    Running,
    /// It ended with this many groups.
    Found(usize),
}

/// For the view's tab: `None` before a comparison was started. While the view is open only
/// groups with two files left count; closed, the files are not looked up and every group found
/// counts.
pub fn groups(app: &App) -> Option<Groups> {
    let run = app.dupes.run.as_ref()?;
    let Some(f) = run.result.get() else {
        return Some(Groups::Running);
    };
    Some(Groups::Found(
        app.dupes.live_groups.unwrap_or(f.groups.len()),
    ))
}

/// A comparison was asked for: it runs or it is done.
pub fn compared(app: &App) -> bool {
    app.dupes.run.is_some()
}

/// The view waits to be asked to compare this machine's files, and the scan is done.
fn waits(app: &App) -> bool {
    active(app) && app.outside.local && app.dupes.run.is_none() && !app.snap.busy
}

/// The view waits, and the files to compare were never counted.
fn wants_count(app: &App) -> bool {
    waits(app) && app.dupes.candidates.is_none()
}

/// Under the read lock, while the view waits to be asked: how many files a comparison would
/// read, and the tree's version. A walk of the scan without building paths, once per change
/// of the tree: passing through the view again counts nothing.
pub fn count(m: &Model, app: &App) -> Option<(u64, usize)> {
    let version = m.tree.version();
    if !waits(app) || app.dupes.counted_at == Some(version) {
        return None;
    }
    Some((
        version,
        match m.tree.roots().next() {
            Some(r) => dfdu_dupes::count_candidates(&m.tree, r, min_size()),
            None => 0,
        },
    ))
}

/// Takes in what [`count`] found, and counts the groups left for the view's tab.
pub fn apply(app: &mut App) {
    if let Some((version, n)) = app.extras.dupe_candidates.take() {
        app.dupes.candidates = Some(n);
        app.dupes.counted_at = Some(version);
    }
    app.dupes.live_groups = found(app)
        .filter(|_| !app.extras.dupes.is_empty())
        .map(|f| {
            let left = |g: &&Group| g.files.iter().filter(|c| live(app, c).is_some()).count() >= 2;
            f.groups.iter().filter(left).count()
        });
}

/// Whether a comparison can start now; when not, the status line says why.
fn can_start(app: &mut App) -> bool {
    // The comparison reads the files here: for another machine's tree it would read this
    // one's files at the same paths, and a delete would then remove the other's.
    if !app.outside.local {
        app.status = Some(
            "duplicates reads this machine's files; this scan is remote or from a file".into(),
        );
        return false;
    }
    if app.snap.busy {
        let again = crate::app::keys::first_key(crate::app::keys::Action::CompareAgain);
        app.status = Some(format!(
            "duplicates: wait until the scan is done, then press {again}"
        ));
        return false;
    }
    true
}

fn start(app: &mut App) -> Vec<Effect> {
    if !can_start(app) {
        return Vec::new();
    }
    app.dupes.cursor = 0;
    app.dupes.scroll = 0;
    app.dupes.toggled.clear();
    app.extras.dupes.clear();
    vec![Effect::FindDupes]
}

/// Starts comparing `cands` (read under the lock by the run loop) in a thread, with as many
/// threads reading as the storage under `root` serves well.
pub fn begin(app: &mut App, root: PathBuf, cands: Vec<Candidate>) {
    let run = Run {
        root,
        progress: Arc::default(),
        cancel: Arc::default(),
        result: Arc::default(),
    };
    let (progress, cancel, result) = (run.progress.clone(), run.cancel.clone(), run.result.clone());
    let root_path = run.root.clone();
    let spawned = std::thread::Builder::new()
        .name("dfdu-dupes".into())
        .spawn(move || {
            let opts = Options::for_storage(dfdu_sys::storage(&root_path));
            let _ = result.set(dfdu_dupes::find(&cands, &opts, &progress, &cancel));
        });
    match spawned {
        Ok(_) => app.dupes.run = Some(run),
        Err(e) => app.status = Some(format!("duplicates: cannot start a thread: {e}")),
    }
}

/// The view needs a read: the files to compare are to be counted, or a result's files have no
/// nodes yet.
pub fn stale(app: &App) -> bool {
    wants_count(app)
        || active(app)
            && found(app).is_some_and(|f| {
                f.groups
                    .iter()
                    .flat_map(|g| &g.files)
                    .any(|c| !app.extras.dupes.contains_key(&c.path))
            })
}

/// Under the read lock: the node of every file of the result, found once by path and then
/// checked to still be in the tree. `None` once a file is gone from the tree (deleted).
pub fn read(m: &Model, app: &App) -> HashMap<PathBuf, Option<NodeRef>> {
    let Some(f) = found(app).filter(|_| active(app)) else {
        return HashMap::new();
    };
    f.groups
        .iter()
        .flat_map(|g| &g.files)
        .map(|c| {
            let node = match app.extras.dupes.get(&c.path) {
                Some(Some(n)) => m.tree.resolve_visible(*n).map(|_| *n),
                Some(None) => None,
                None => dfdu_tree::find_path(&m.tree, &HostPath::local(&c.path))
                    .filter(|&ix| m.tree.dev_ino(ix) == c.dev_ino)
                    .map(|ix| m.tree.node_ref(ix)),
            };
            (c.path.clone(), node)
        })
        .collect()
}

/// The path of the selected file, for `y` and `o`; `None` on a group's row.
pub(crate) fn selected_path(app: &App) -> Option<PathBuf> {
    match rows(app).get(app.dupes.cursor)? {
        &Row::File(g, i) => Some(found(app)?.groups[g].files[i].path.clone()),
        Row::Group(_) => None,
    }
}

pub fn key(app: &mut App, k: &KeyEvent) -> Option<Vec<Effect>> {
    if app.dupes.run.is_none() && k.code == KeyCode::Enter {
        return Some(start(app));
    }
    let rows = rows(app);
    let last = rows.len().saturating_sub(1);
    let page = app.list_height.max(1);
    let action = keys::action_for(k)?;
    if let Some(c) = keys::cursor_after(action, app.dupes.cursor, last, page) {
        app.dupes.cursor = c;
        return Some(Vec::new());
    }
    let d = &mut app.dupes;
    match action {
        Action::ToggleMark => {
            let Some(f) = found(app) else {
                return Some(Vec::new());
            };
            match rows.get(app.dupes.cursor) {
                Some(&Row::File(group, file)) => {
                    let c = f.groups[group].files[file].clone();
                    if let Some(n) = live(app, &c) {
                        let on = !app.marks.contains(n);
                        marks::set(app, mark_of(&c, n), on);
                    }
                    app.dupes.cursor = (app.dupes.cursor + 1).min(last);
                }
                Some(&Row::Group(group)) => {
                    let picks = copies(app, &f.groups[group]);
                    let on = !picks.iter().all(|(_, n)| app.marks.contains(*n));
                    for (c, n) in &picks {
                        marks::set(app, mark_of(c, *n), on);
                    }
                }
                None => {}
            }
        }
        Action::Open | Action::Enter => match rows.get(d.cursor) {
            Some(&Row::Group(g)) => {
                if !d.toggled.remove(&g) {
                    d.toggled.insert(g);
                }
            }
            Some(&Row::File(g, i)) => {
                let f = found(app)?;
                let (path, root) = (
                    f.groups[g].files[i].path.clone(),
                    app.dupes.run.as_ref()?.root.clone(),
                );
                views::switch(app, View::Files);
                app.go_to(&HostPath::local(&path), &HostPath::local(&root));
            }
            None => {}
        },
        Action::Parent => {
            let at = (0..=d.cursor.min(last))
                .rev()
                .find(|&i| matches!(rows.get(i), Some(Row::Group(_))));
            if let Some(i) = at
                && let Row::Group(g) = rows[i]
                && let Some(group) = found(app).map(|f| &f.groups[g])
            {
                if open(app, g, group) {
                    app.dupes.toggled.insert(g);
                }
                app.dupes.cursor = i;
            }
        }
        _ => return None,
    }
    Some(Vec::new())
}

/// `R` ([`Action::CompareAgain`](crate::app::keys::Action)): stops the comparison that runs,
/// if one does, and starts another.
pub fn compare_again(app: &mut App) -> Vec<Effect> {
    // What there is stays shown when a new comparison cannot start.
    if !can_start(app) {
        return Vec::new();
    }
    if let Some(r) = app.dupes.run.take() {
        r.cancel.store(true, Ordering::Relaxed);
    }
    start(app)
}

/// `A` ([`Action::TickSuggested`](crate::app::keys::Action)): marks every copy but the one
/// kept in each group that frees something.
pub fn tick_suggested(app: &mut App) {
    if app.read_only {
        app.status = Some(crate::app::keys::READ_ONLY.into());
        return;
    }
    let n = mark_all(app);
    let keep = app.dupes.keep.label();
    app.status = Some(match n {
        0 => "every copy is marked already, or there are none to free".into(),
        1 => format!("marked 1 copy (the {keep} file of each group is kept)"),
        n => format!("marked {n} copies (the {keep} file of each group is kept)"),
    });
}

#[cfg(test)]
mod tests;
