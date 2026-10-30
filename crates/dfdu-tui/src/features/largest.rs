//! The largest files below (`5`), as broot's whale mode: the largest files anywhere under the
//! current directory in one flat list, with their paths relative to it.
//!
//! Only while the view is open, the extras read walks the directory's subtree under the read
//! lock and keeps the largest [`TOP`] files in a bounded heap. It walks again when the view
//! opens, for another directory or size mode, and when the scan data changed: at once when no
//! scan is running (a delete), and during a scan at most every [`MIN_INTERVAL`] and at most a
//! tenth of the time, as the run loop paces the snapshot. It is one of the review views
//! (`features::review`): `Space` ticks a file (marks it, as in the list), `Enter` shows it in
//! Files, in its directory, and `d` deletes what is ticked.

use std::cmp::Reverse;
use std::collections::BinaryHeap;
use std::time::{Duration, Instant};

use dfdu_engine::{Model, SizeMode};
use dfdu_tree::{HostPath, Ix, Kind, NodeRef, Tree};
use ratatui::crossterm::event::KeyEvent;

use crate::app::keys::{self, Action};
use crate::app::{App, Effect};
use crate::model::snapshot::{Ident, Target};
use crate::state::marks;

mod draw;

pub use draw::draw;

/// The most files the view lists.
pub const TOP: usize = 1000;
/// During a scan, the walk runs at most this often.
const MIN_INTERVAL: Duration = Duration::from_secs(1);

/// What the view keeps between frames.
#[derive(Clone, Debug, Default)]
pub struct Largest {
    pub on: bool,
    cursor: usize,
    scroll: usize,
    /// The files the last walk found, largest first.
    files: Vec<File>,
    /// The files below the directory, all of them, as the last walk counted them.
    count: u64,
    /// What the last walk was for; `None` until a walk for the view as opened came in.
    walked: Option<Walked>,
}

/// What a walk was for, and when it ran.
#[derive(Clone, Debug)]
pub struct Walked {
    dir: HostPath,
    mode: SizeMode,
    hide_hidden: bool,
    at: Instant,
    /// How long it held the read lock.
    cost: Duration,
    /// The model's fingerprint then.
    fingerprint: u64,
}

/// One of the largest files.
#[derive(Clone, Debug)]
pub struct File {
    pub node: NodeRef,
    /// Relative to the directory walked.
    pub path: HostPath,
    pub size: u64,
    pub dev_ino: (u64, u64),
    pub shared_out: u64,
}

/// What [`read`] found.
#[derive(Clone, Debug)]
pub struct Info {
    files: Vec<File>,
    count: u64,
    walked: Walked,
}

/// True when the view is shown, and so takes the keys it knows.
pub fn active(app: &App) -> bool {
    app.largest.on
}

/// Switches the view on, the other views off, or the view off. On, it walks again before the
/// next frame, so what it shows is never older than the opening.
pub fn show(app: &mut App, on: bool) {
    app.largest.on = on;
    if on {
        app.largest.walked = None;
        crate::features::dupes::show(app, false);
        crate::features::cleanup::show(app, false);
        crate::features::treemap::show(app, false);
    }
}

/// The view needs a walk at once: it was just opened, or shows another directory, size mode
/// or choice of hidden files than the last walk was for.
pub fn stale(app: &App) -> bool {
    app.largest.on
        && !app.largest.walked.as_ref().is_some_and(|w| {
            w.dir == app.snap.dir_path
                && w.mode == app.size_mode
                && w.hide_hidden == app.hide_hidden
        })
}

/// Under the read lock, while the view is open and a walk is due: the largest [`TOP`] files
/// below the current directory. `None` keeps what the last walk found.
pub fn read(m: &Model, app: &App) -> Option<Info> {
    if !app.largest.on {
        return None;
    }
    let fingerprint = m.fingerprint();
    if !stale(app) {
        let w = app.largest.walked.as_ref()?;
        let paced = !app.snap.busy || w.at.elapsed() >= MIN_INTERVAL.max(w.cost * 9);
        if w.fingerprint == fingerprint || !paced {
            return None;
        }
    }
    let at = Instant::now();
    let t = &m.tree;
    let (files, count) = match app.snap.dir_node.and_then(|r| t.resolve_visible(r)) {
        Some(dir) => walk(t, dir, app.size_mode, app.hide_hidden),
        None => (Vec::new(), 0),
    };
    Some(Info {
        files,
        count,
        walked: Walked {
            dir: app.snap.dir_path.clone(),
            mode: app.size_mode,
            hide_hidden: app.hide_hidden,
            at,
            cost: at.elapsed(),
            fingerprint,
        },
    })
}

/// The largest [`TOP`] files below `dir` in size mode `mode`, largest first and by path among
/// equals, and how many files there are below it. A walk of the subtree with a heap of at most
/// [`TOP`] files: the cost is one look at each node, and the paths are built only for those
/// kept. `hide_hidden` leaves out hidden files and everything in hidden directories.
fn walk(t: &Tree, dir: Ix, mode: SizeMode, hide_hidden: bool) -> (Vec<File>, u64) {
    let mut heap: BinaryHeap<Reverse<(u64, Ix)>> = BinaryHeap::with_capacity(TOP + 1);
    let mut count = 0;
    let mut stack = vec![dir];
    while let Some(d) = stack.pop() {
        for c in t.children(d) {
            if hide_hidden && t.name(c).first() == Some(&b'.') {
                continue;
            }
            if t.dir(c).is_some() {
                stack.push(c);
                continue;
            }
            if t.kind(c) != Kind::File {
                continue;
            }
            count += 1;
            let size = mode.of(&t.total(c));
            if heap.len() < TOP {
                heap.push(Reverse((size, c)));
            } else if heap.peek().is_some_and(|Reverse((least, _))| size > *least) {
                heap.pop();
                heap.push(Reverse((size, c)));
            }
        }
    }
    let mut files: Vec<File> = (heap.into_iter())
        .map(|Reverse((size, ix))| File {
            node: t.node_ref(ix),
            path: below(t, dir, ix),
            size,
            dev_ino: t.dev_ino(ix),
            shared_out: t.total(ix).shared_out,
        })
        .collect();
    files.sort_by(|a, b| {
        (b.size.cmp(&a.size)).then_with(|| a.path.components().cmp(b.path.components()))
    });
    (files, count)
}

/// The path of `ix` relative to its ancestor `dir`.
fn below(t: &Tree, dir: Ix, ix: Ix) -> HostPath {
    let mut names = Vec::new();
    let mut at = Some(ix);
    while let Some(i) = at.filter(|&i| i != dir) {
        names.push(t.name(i));
        at = t.parent(i);
    }
    let mut out = HostPath::empty(t.host());
    for n in names.iter().rev() {
        out.push(n);
    }
    out
}

/// Takes in a walk, keeping the cursor on the file it was on when that file is still listed,
/// and at the top for another directory or a view just opened.
pub fn apply(app: &mut App) {
    let Some(info) = app.extras.largest.take() else {
        return;
    };
    let l = &mut app.largest;
    let same_dir = (l.walked.as_ref()).is_some_and(|w| w.dir == info.walked.dir);
    let was = (l.files.get(l.cursor).map(|f| f.node)).filter(|_| same_dir);
    if !same_dir {
        (l.cursor, l.scroll) = (0, 0);
    }
    l.files = info.files;
    l.count = info.count;
    l.walked = Some(info.walked);
    l.cursor = was
        .and_then(|n| l.files.iter().position(|f| f.node == n))
        .unwrap_or_else(|| l.cursor.min(l.files.len().saturating_sub(1)));
}

/// The selected file, its path joined to the directory shown.
pub(crate) fn selected(app: &App) -> Option<(HostPath, &File)> {
    let f = app.largest.files.get(app.largest.cursor)?;
    Some((app.snap.dir_path.join(f.path.as_bytes()), f))
}

/// Keys the view takes before the action registry; `None` passes the key on. The keys that
/// delete act on what is marked, never on the list's selection hidden behind the view.
pub fn key(app: &mut App, k: &KeyEvent) -> Option<Vec<Effect>> {
    let last = app.largest.files.len().saturating_sub(1);
    let page = app.list_height.max(1);
    let action = keys::action_for(k)?;
    if let Some(c) = keys::cursor_after(action, app.largest.cursor, last, page) {
        app.largest.cursor = c;
        return Some(Vec::new());
    }
    match action {
        Action::Open | Action::Enter => go(app),
        Action::ToggleMark => toggle(app),
        _ => return None,
    }
    Some(Vec::new())
}

/// The nodes of the files the view lists, for `d` to act on its ticks only.
pub fn listed(app: &App) -> std::collections::HashSet<NodeRef> {
    app.largest.files.iter().map(|f| f.node).collect()
}

/// Goes to the selected file: closes the view, opens the file's directory and selects it there.
fn go(app: &mut App) {
    let Some((path, _)) = selected(app) else {
        return;
    };
    let dir = app.snap.dir_path.clone();
    app.largest.on = false;
    app.go_to(&path, &dir);
}

/// Marks or unmarks the selected file and moves down a row.
fn toggle(app: &mut App) {
    let Some((path, f)) = selected(app) else {
        return;
    };
    let item = marks::Item {
        target: Target {
            node: Some(f.node),
            path,
            ident: Some(Ident {
                dev_ino: f.dev_ino,
                kind: Kind::File,
            }),
        },
        is_dir: false,
        size: f.size,
        items: 1,
        shared_out: f.shared_out,
        scanning: false,
        inside: false,
    };
    let on = !app.marks.contains(f.node);
    marks::set(app, item, on);
    let last = app.largest.files.len().saturating_sub(1);
    app.largest.cursor = (app.largest.cursor + 1).min(last);
}

#[cfg(test)]
mod tests;
