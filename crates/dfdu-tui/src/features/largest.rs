//! The largest files below (`5`), as broot's whale mode: the largest files anywhere under the
//! current directory in one flat list, with their paths relative to it.
//!
//! Only while the view is open, the extras read walks the directory's subtree under the read
//! lock and keeps the largest [`TOP`](dfdu_session::largest::TOP) files in a bounded heap. It
//! walks again when the view opens, for another directory or size mode, and when the scan data
//! changed: at once when no scan is running (a delete), and during a scan at most every
//! [`MIN_INTERVAL`] and at most a tenth of the time, as the run loop paces the snapshot. It is
//! one of the review views (`features::review`): `Space` ticks a file (marks it, as in the
//! list), `Enter` shows it in Files, in its directory, and `d` deletes what is ticked.

use std::collections::HashSet;
use std::time::{Duration, Instant};

use dfdu_engine::{Model, SizeMode};
pub use dfdu_session::largest::File;
use dfdu_session::largest::walk;
use dfdu_tree::{HostPath, Ident, Kind, NodeRef, Target};
use ratatui::crossterm::event::KeyEvent;

use crate::app::keys::{self, Action};
use crate::app::views::{self, View};
use crate::app::{App, Effect};
use crate::state::marks;

mod draw;

pub use draw::draw;

/// During a scan, the walk runs at most this often.
const MIN_INTERVAL: Duration = Duration::from_secs(1);

/// What the view keeps between frames.
#[derive(Clone, Debug, Default)]
pub struct Largest {
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

/// What [`read`] found.
#[derive(Clone, Debug)]
pub struct Info {
    files: Vec<File>,
    count: u64,
    walked: Walked,
}

/// True when the view is shown, and so takes the keys it knows.
pub fn active(app: &App) -> bool {
    views::current(app) == View::Largest
}

/// The view was opened ([`views::switch`]): it walks again before the next frame, so what it
/// shows is never older than the opening.
pub fn on_open(app: &mut App) {
    app.largest.walked = None;
}

/// The view needs a walk at once: it was just opened, or shows another directory, size mode
/// or choice of hidden files than the last walk was for.
pub fn stale(app: &App) -> bool {
    active(app)
        && !app.largest.walked.as_ref().is_some_and(|w| {
            w.dir == app.snap.dir_path
                && w.mode == app.size_mode
                && w.hide_hidden == app.hide_hidden
        })
}

/// Under the read lock, while the view is open and a walk is due: the largest files below the
/// current directory ([`walk`]). `None` keeps what the last walk found.
pub fn read(m: &Model, app: &App) -> Option<Info> {
    if !active(app) {
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
pub fn selected(app: &App) -> Option<(HostPath, &File)> {
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
        Action::Open | Action::Enter => show_in_files(app),
        Action::ToggleMark => toggle(app),
        _ => return None,
    }
    Some(Vec::new())
}

/// The nodes of the files the view lists, for `d` to act on its ticks only.
pub fn listed(app: &App) -> HashSet<NodeRef> {
    app.largest.files.iter().map(|f| f.node).collect()
}

/// Goes to the selected file: closes the view, opens the file's directory and selects it there.
fn show_in_files(app: &mut App) {
    let Some((path, _)) = selected(app) else {
        return;
    };
    let dir = app.snap.dir_path.clone();
    views::switch(app, View::Files);
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
