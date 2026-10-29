//! The duplicates view (`4`): files with identical contents below the scan root, found on
//! request with [`dfdu_dupes`] once the scan is done.
//!
//! Switching the view on starts the comparison in a thread of its own, from the candidates read
//! under the read lock ([`Effect::FindDupes`]); the view shows its progress and then the groups,
//! the most space to free first. Marks are the list's: `Space` marks a copy, or on a group every
//! copy but the one kept, `a` does that for every group, and `d` and `D` delete what is marked
//! through the usual dialog, which checks each file against the scan. The copy kept is the
//! oldest, and `K` cycles the rule ([`Keep`]). Groups of APFS clones, which free nothing, are
//! listed but `a` leaves them alone.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock};

use dfdu_dupes::{Candidate, Found, Group, Options, Progress};
use dfdu_engine::Model;
use dfdu_tree::{HostPath, Kind, NodeRef};
use ratatui::crossterm::event::{KeyCode, KeyEvent};

use crate::app::{App, Effect};
use crate::model::snapshot::{Ident, Target};
use crate::state::marks;

mod draw;

pub use draw::draw;

/// Files smaller than this are not compared: the size `dfdu dupes` starts from.
pub fn min_size() -> u64 {
    Options::default().min_size
}

/// What the view keeps between frames.
#[derive(Clone, Debug, Default)]
pub struct Dupes {
    pub on: bool,
    cursor: usize,
    scroll: usize,
    run: Option<Run>,
    keep: Keep,
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

/// The next keep rule, with the status line saying so.
pub fn cycle_keep(app: &mut App) {
    let d = &mut app.dupes;
    d.keep = d.keep.next();
    app.status = Some(format!(
        "duplicates: keep the {} copy (a marks the others)",
        d.keep.label()
    ));
}

/// A comparison, running or done.
#[derive(Clone, Debug)]
struct Run {
    root: PathBuf,
    progress: Arc<Progress>,
    cancel: Arc<AtomicBool>,
    result: Arc<OnceLock<Found>>,
}

pub fn active(app: &App) -> bool {
    app.dupes.on
}

/// A comparison is running: the view's progress line needs frames.
pub fn running(app: &App) -> bool {
    app.dupes.on
        && app
            .dupes
            .run
            .as_ref()
            .is_some_and(|r| r.result.get().is_none())
}

fn found(app: &App) -> Option<&Found> {
    app.dupes.run.as_ref()?.result.get()
}

/// For the view's tab: `None` before a comparison was started, `Some(None)` while one runs,
/// and then the number of groups. While the view is open only groups with two files left
/// count; closed, the files are not looked up and every group found counts.
pub fn groups(app: &App) -> Option<Option<usize>> {
    let run = app.dupes.run.as_ref()?;
    let Some(f) = run.result.get() else {
        return Some(None);
    };
    if app.extras.dupes.is_empty() {
        return Some(Some(f.groups.len()));
    }
    let live = |g: &Group| g.files.iter().filter(|c| live(app, c).is_some()).count() >= 2;
    Some(Some(f.groups.iter().filter(|g| live(g)).count()))
}

/// Switches the view on or off. On, it starts a comparison unless one ran already.
pub fn show(app: &mut App, on: bool) -> Vec<Effect> {
    app.dupes.on = on;
    if !on {
        return Vec::new();
    }
    crate::features::treemap::show(app, false);
    crate::features::cleanup::show(app, false);
    if app.dupes.run.is_none() {
        return start(app);
    }
    Vec::new()
}

fn start(app: &mut App) -> Vec<Effect> {
    // The comparison reads the files here: for another machine's tree it would read this
    // one's files at the same paths, and a delete would then remove the other's.
    if !app.outside.local {
        app.status = Some(
            "duplicates reads this machine's files; this scan is remote or from a file".into(),
        );
        return Vec::new();
    }
    if app.snap.busy {
        app.status = Some("duplicates: wait until the scan is done, then press r".into());
        return Vec::new();
    }
    app.dupes.cursor = 0;
    app.dupes.scroll = 0;
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

/// The view needs a read: a result whose files have no nodes yet.
pub fn stale(app: &App) -> bool {
    app.dupes.on
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
    let Some(f) = found(app).filter(|_| app.dupes.on) else {
        return HashMap::new();
    };
    f.groups
        .iter()
        .flat_map(|g| &g.files)
        .map(|c| {
            let node = match app.extras.dupes.get(&c.path) {
                Some(Some(n)) => m.tree.resolve_visible(*n).map(|_| *n),
                Some(None) => None,
                None => crate::model::snapshot::find_path(&m.tree, &HostPath::local(&c.path))
                    .filter(|&ix| m.tree.dev_ino(ix) == c.dev_ino)
                    .map(|ix| m.tree.node_ref(ix)),
            };
            (c.path.clone(), node)
        })
        .collect()
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
enum Row {
    Group(usize),
    File(usize, usize),
}

/// A file still in the tree.
fn live(app: &App, c: &Candidate) -> Option<NodeRef> {
    app.extras.dupes.get(&c.path).copied().flatten()
}

/// The groups with at least two files left, each followed by its files.
fn rows(app: &App) -> Vec<Row> {
    let Some(f) = found(app) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for (g, group) in f.groups.iter().enumerate() {
        let left: Vec<usize> = (0..group.files.len())
            .filter(|&i| live(app, &group.files[i]).is_some())
            .collect();
        if left.len() < 2 {
            continue;
        }
        out.push(Row::Group(g));
        out.extend(left.into_iter().map(|i| Row::File(g, i)));
    }
    out
}

fn mark_of(c: &Candidate, node: NodeRef) -> marks::Item {
    marks::Item {
        target: Target {
            node: Some(node),
            path: HostPath::local(&c.path),
            ident: Some(Ident {
                dev_ino: c.dev_ino,
                kind: Kind::File,
            }),
        },
        is_dir: false,
        size: c.freed(),
        items: 1,
        shared_out: 0,
        scanning: false,
        inside: false,
    }
}

/// The index in `group` of the file still in the tree that the keep rule keeps.
fn kept(app: &App, group: &Group) -> Option<usize> {
    let left = (group.files.iter().enumerate()).filter(|(_, c)| live(app, c).is_some());
    let by_path = |c: &Candidate| c.path.clone();
    match app.dupes.keep {
        Keep::Oldest => left.min_by_key(|(_, c)| (c.mtime, by_path(c))),
        Keep::Newest => left.min_by_key(|(_, c)| (std::cmp::Reverse(c.mtime), by_path(c))),
        Keep::ShortestPath => left.min_by_key(|(_, c)| (c.path.as_os_str().len(), by_path(c))),
        Keep::FirstPath => left.min_by_key(|(_, c)| by_path(c)),
    }
    .map(|(i, _)| i)
}

/// The files of `group` still in the tree, but the one [`kept`].
fn copies(app: &App, group: &Group) -> Vec<(Candidate, NodeRef)> {
    let keep = kept(app, group);
    (group.files.iter().enumerate())
        .filter(|&(i, _)| Some(i) != keep)
        .filter_map(|(_, c)| Some((c.clone(), live(app, c)?)))
        .collect()
}

/// Marks every copy but the one kept in each group that frees something; returns how many were
/// newly marked.
fn mark_all(app: &mut App) -> usize {
    let Some(f) = found(app) else {
        return 0;
    };
    let picks: Vec<(Candidate, NodeRef)> = f
        .groups
        .iter()
        .filter(|g| !g.all_clones())
        .flat_map(|g| copies(app, g))
        .filter(|(_, n)| !app.marks.contains(*n))
        .collect();
    for (c, n) in &picks {
        marks::set(app, mark_of(c, *n), true);
    }
    picks.len()
}

/// The path of the selected file, for `y` and `o`; `None` on a group's row.
pub(crate) fn selected_path(app: &App) -> Option<PathBuf> {
    match rows(app).get(app.dupes.cursor)? {
        &Row::File(g, i) => Some(found(app)?.groups[g].files[i].path.clone()),
        Row::Group(_) => None,
    }
}

pub fn key(app: &mut App, k: &KeyEvent) -> Option<Vec<Effect>> {
    let rows = rows(app);
    let last = rows.len().saturating_sub(1);
    let page = app.list_height.max(1);
    let d = &mut app.dupes;
    if let Some(n) = crate::app::keys::half_page(k, page) {
        d.cursor = d.cursor.saturating_add_signed(n).min(last);
        return Some(Vec::new());
    }
    match k.code {
        KeyCode::Up | KeyCode::Char('k') => d.cursor = d.cursor.saturating_sub(1),
        KeyCode::Down | KeyCode::Char('j') => d.cursor = (d.cursor + 1).min(last),
        KeyCode::PageUp => d.cursor = d.cursor.saturating_sub(page),
        KeyCode::PageDown => d.cursor = (d.cursor + page).min(last),
        KeyCode::Home | KeyCode::Char('g') => d.cursor = 0,
        KeyCode::End | KeyCode::Char('G') => d.cursor = last,
        KeyCode::Char('r') => {
            if let Some(r) = d.run.take() {
                r.cancel.store(true, Ordering::Relaxed);
            }
            return Some(start(app));
        }
        KeyCode::Char('a') if app.read_only => {
            app.status = Some(crate::app::keys::READ_ONLY.into());
        }
        KeyCode::Char('a') => {
            let n = mark_all(app);
            let keep = app.dupes.keep.label();
            app.status = Some(match n {
                0 => "every copy is marked already, or there are none to free".into(),
                1 => format!("marked 1 copy (the {keep} file of each group is kept)"),
                n => format!("marked {n} copies (the {keep} file of each group is kept)"),
            });
        }
        KeyCode::Char(' ') => {
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
        KeyCode::Enter | KeyCode::Right | KeyCode::Char('l') => {
            let f = found(app)?;
            if let Some(&Row::File(g, i)) = rows.get(app.dupes.cursor) {
                let (path, root) = (
                    f.groups[g].files[i].path.clone(),
                    app.dupes.run.as_ref()?.root.clone(),
                );
                app.dupes.on = false;
                app.go_to(&HostPath::local(&path), &HostPath::local(&root));
            }
        }
        _ => return None,
    }
    Some(Vec::new())
}

#[cfg(test)]
mod tests;
