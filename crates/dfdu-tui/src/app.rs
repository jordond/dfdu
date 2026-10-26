//! UI state and its update function (`docs/research/tui-design.md` §2.2). Keys turn into
//! changes of [`App`] plus effects for the run loop to carry out; [`App::refresh`] folds a new
//! [`Snapshot`] into the state and applies the row-stability rules.

use std::cell::Cell;
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::time::{Duration, Instant};

use dfdu_engine::{Command, Mode};
use dfdu_tree::NodeRef;
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};

use crate::order::{Order, SortKey, SortSpec};
use crate::problems::ErrorList;
use crate::snapshot::{EntryData, SizeMode, Snapshot, Target};
use crate::theme::Theme;

/// Automatic re-sort after this long without input...
const IDLE_BEFORE_SORT: Duration = Duration::from_millis(1500);
/// ...and at most this often.
const MIN_SORT_INTERVAL: Duration = Duration::from_secs(2);

#[derive(Debug)]
pub enum Effect {
    Send(Command),
    /// Read the error list from the engine and pass it to [`App::show_errors`].
    ShowErrors,
}

#[derive(Clone, Debug)]
pub enum Overlay {
    None,
    Help,
    /// What could not be read, with a cursor.
    Errors {
        list: ErrorList,
        cursor: usize,
    },
    ConfirmDelete {
        node: NodeRef,
        name: String,
        size: u64,
        /// Part of `size` that data outside the scan also uses.
        shared_out: u64,
        is_dir: bool,
        mode: Mode,
        /// What the user typed so far (permanent delete asks for the word "delete").
        typed: String,
        /// The directory is still being scanned; its size is a lower bound.
        scanning: bool,
    },
}

/// The selected entry: its reference, plus name and identity to find it again after a rescan.
#[derive(Clone, Debug)]
pub struct Selection {
    /// `None` when only the name is known (going up to the parent directory).
    pub node: Option<NodeRef>,
    pub name: Vec<u8>,
    pub dev_ino: (u64, u64),
}

impl Selection {
    fn of(e: &EntryData) -> Self {
        Selection {
            node: Some(e.node),
            name: e.name.to_vec(),
            dev_ino: e.dev_ino,
        }
    }
}

/// Remembered position in a directory, restored when the user comes back to it.
#[derive(Clone, Debug)]
struct Cursor {
    selected: Option<Selection>,
    scroll: usize,
}

pub struct App {
    pub theme: Theme,
    pub target: Target,
    pub snap: Snapshot,
    pub order: Order,
    pub sort: SortSpec,
    pub size_mode: SizeMode,
    pub hide_hidden: bool,
    pub selected: Option<Selection>,
    /// Indices into `snap.entries`, in display order.
    pub rows: Vec<usize>,
    /// Entries that arrived since the last sort, shown as one summary row.
    pub new_count: usize,
    pub new_largest: u64,
    /// Largest size among `rows`, for the bars.
    pub rows_largest: u64,
    pub scroll: usize,
    pub list_height: usize,
    pub overlay: Overlay,
    pub status: Option<String>,
    pub started: Instant,
    pub done_at: Option<Instant>,
    pub quit: bool,
    last_input: Instant,
    last_sort: Instant,
    sort_requested: bool,
    changed_since_sort: bool,
    /// The user moved the cursor in the current directory; re-sorts then follow the selected
    /// row instead of returning to the top.
    moved: bool,
    cursors: HashMap<PathBuf, Cursor>,
    /// Row where [`App::selected_index`] last found the selection.
    sel_hint: Cell<usize>,
}

/// One line describing a finished delete plan. On btrfs the freed space shows up in the free
/// space only after the transaction commits, and for snapshots after the cleaner ran (B11).
pub fn delete_summary(d: &dfdu_engine::DeleteSummary) -> String {
    let mut parts = vec![format!(
        "deleted {} ({})",
        d.deleted,
        crate::theme::human(d.bytes)
    )];
    for (n, what) in [
        (d.stale, "changed since the scan"),
        (d.refused, "refused"),
        (d.failed, "failed"),
        (d.aborted, "stopped"),
    ] {
        if n > 0 {
            parts.push(format!("{n} {what}"));
        }
    }
    let mut s = parts.join(", ");
    if d.btrfs {
        s.push_str("; btrfs frees the space when it commits (snapshots: in the background)");
    }
    s
}

impl App {
    pub fn new(theme: Theme, now: Instant) -> Self {
        App {
            theme,
            target: Target::root(),
            snap: Snapshot::default(),
            order: Order::default(),
            sort: SortSpec::default(),
            size_mode: SizeMode::Disk,
            hide_hidden: false,
            selected: None,
            rows: Vec::new(),
            new_count: 0,
            new_largest: 0,
            rows_largest: 0,
            scroll: 0,
            list_height: 1,
            overlay: Overlay::None,
            status: None,
            started: now,
            done_at: None,
            quit: false,
            // No key pressed yet counts as idle.
            last_input: now.checked_sub(IDLE_BEFORE_SORT).unwrap_or(now),
            last_sort: now,
            sort_requested: true,
            changed_since_sort: false,
            moved: false,
            cursors: HashMap::new(),
            sel_hint: Cell::new(0),
        }
    }

    pub fn selected_index(&self) -> Option<usize> {
        let node = self.selected.as_ref()?.node?;
        let is_sel = |&i: &usize| self.snap.entries[i].node == node;
        // Usually the row found last time; a directory can have a million rows.
        let hint = self.sel_hint.get();
        if self.rows.get(hint).is_some_and(is_sel) {
            return Some(hint);
        }
        let found = self.rows.iter().position(is_sel)?;
        self.sel_hint.set(found);
        Some(found)
    }

    pub fn selected_entry(&self) -> Option<&EntryData> {
        self.selected_index()
            .map(|i| &self.snap.entries[self.rows[i]])
    }

    /// True when an automatic re-sort is waiting for the user to be idle.
    pub fn sort_pending(&self) -> bool {
        self.new_count > 0 || self.changed_since_sort
    }

    /// When the run loop should wake for an automatic re-sort, if one is pending.
    pub fn next_sort_at(&self) -> Option<Instant> {
        self.sort_pending()
            .then(|| (self.last_input + IDLE_BEFORE_SORT).max(self.last_sort + MIN_SORT_INTERVAL))
    }

    fn auto_sort_due(&self, now: Instant) -> bool {
        self.sort_pending()
            && now.duration_since(self.last_input) >= IDLE_BEFORE_SORT
            && now.duration_since(self.last_sort) >= MIN_SORT_INTERVAL
    }

    /// Folds a new snapshot into the state.
    pub fn refresh(&mut self, snap: Snapshot, now: Instant) {
        if let Some(d) = &snap.last_delete
            && self
                .snap
                .last_delete
                .as_ref()
                .is_none_or(|p| p.plan != d.plan)
        {
            self.status = Some(delete_summary(d));
        }
        let dir_changed = snap.dir_path != self.snap.dir_path;
        // While a scan runs, entries are only added: the old entries are a prefix of the new
        // ones and indices into them stay valid, so nothing needs to be looked up by node.
        let appended = !dir_changed
            && snap.entries.len() >= self.snap.entries.len()
            && self
                .snap
                .entries
                .iter()
                .zip(&snap.entries)
                .all(|(a, b)| a.node == b.node);
        let swapped = if dir_changed || appended {
            HashMap::new()
        } else {
            self.carry_over_swapped(&snap)
        };
        if snap.version != self.snap.version {
            self.changed_since_sort = true;
        }
        // A scan that just finished gets one more automatic sort right away (if the user is
        // idle), so the final order does not wait for the minimum interval.
        let finished = self.snap.busy && !snap.busy;
        if self.done_at.is_none() && !snap.busy && snap.dir_node.is_some() {
            self.done_at = Some(now);
        }
        if self.done_at.is_some() && snap.busy && !self.snap.busy {
            // A rescan started: time it from now.
            self.started = now;
            self.done_at = None;
        }
        let old_index = self.selected_index();
        let old = std::mem::replace(&mut self.snap, snap);
        if !dir_changed && !appended {
            let pos: HashMap<NodeRef, usize> = self
                .snap
                .entries
                .iter()
                .enumerate()
                .map(|(i, e)| (e.node, i))
                .collect();
            self.order.remap(|i| {
                let node = old.entries[i].node;
                pos.get(swapped.get(&node).unwrap_or(&node)).copied()
            });
        }
        drop(old);
        self.update_order(true, dir_changed, finished, old_index, now);
    }

    /// Applies sort requests and the automatic sort to the snapshot already held, for a frame
    /// drawn without reading the engine again.
    pub fn reorder(&mut self, now: Instant) {
        let old_index = self.selected_index();
        self.update_order(false, false, false, old_index, now);
    }

    /// Sorts or updates the row order, then puts the selection and scroll position back.
    /// `fresh`: a new snapshot was just folded in.
    fn update_order(
        &mut self,
        fresh: bool,
        dir_changed: bool,
        finished: bool,
        old_index: Option<usize>,
        now: Instant,
    ) {
        let hide = self.hide_hidden;
        let visible = |e: &EntryData| !(hide && e.hidden());
        let entries = &self.snap.entries;
        let requested = std::mem::take(&mut self.sort_requested);
        let resorted = if dir_changed || requested {
            self.order = Order::default();
            self.order.sort_exact(entries, visible, self.sort);
            true
        } else if self.auto_sort_due(now)
            || (finished && now.duration_since(self.last_input) >= IDLE_BEFORE_SORT)
        {
            self.order.sort_auto(entries, visible, self.sort);
            true
        } else if fresh {
            self.order.sync(entries, visible, self.sort);
            false
        } else {
            // The entries and the order are those of the last frame: nothing below changes.
            return;
        };
        if resorted {
            self.last_sort = now;
            self.changed_since_sort = false;
        }

        self.rows.clear();
        self.rows.extend_from_slice(self.order.rows());
        self.new_count = self.order.new_entries().len();
        self.new_largest = self
            .order
            .new_entries()
            .iter()
            .map(|&i| entries[i].size)
            .max()
            .unwrap_or(0);
        self.rows_largest = self
            .rows
            .iter()
            .map(|&i| entries[i].size)
            .max()
            .unwrap_or(0);

        self.fix_selection(old_index);
        if resorted && !dir_changed {
            if self.moved {
                // Keep the selected row on the same screen line.
                if let (Some(old), Some(new)) = (old_index, self.selected_index()) {
                    let line = old.saturating_sub(self.scroll);
                    self.scroll = new.saturating_sub(line);
                }
            } else {
                // The user has not moved the cursor here: stay at the top.
                self.select(0);
                self.scroll = 0;
            }
        }
    }

    /// A rescan replaces nodes. Rows whose node disappeared take over the new node with the same
    /// name and identity, so they keep their position instead of turning into "new" entries.
    /// Returns the replaced nodes, old to new.
    fn carry_over_swapped(&mut self, snap: &Snapshot) -> HashMap<NodeRef, NodeRef> {
        let mut swapped = HashMap::new();
        let present: HashSet<NodeRef> = snap.entries.iter().map(|e| e.node).collect();
        let before: HashSet<NodeRef> = self.snap.entries.iter().map(|e| e.node).collect();
        let fresh: HashMap<(&[u8], (u64, u64)), NodeRef> = snap
            .entries
            .iter()
            .filter(|e| !before.contains(&e.node))
            .map(|e| ((&e.name[..], e.dev_ino), e.node))
            .collect();
        if fresh.is_empty() {
            return swapped;
        }
        for old in self
            .snap
            .entries
            .iter()
            .filter(|e| !present.contains(&e.node))
        {
            if let Some(&new) = fresh.get(&(&old.name[..], old.dev_ino)) {
                swapped.insert(old.node, new);
                if let Some(sel) = self.selected.as_mut().filter(|s| s.node == Some(old.node)) {
                    sel.node = Some(new);
                }
            }
        }
        swapped
    }

    /// Re-finds the selection after a refresh: by reference, then by name and identity (rescan
    /// swap), then by the old position.
    fn fix_selection(&mut self, old_index: Option<usize>) {
        if self.selected_index().is_some() {
            return;
        }
        if let Some(sel) = &self.selected {
            let found = self.rows.iter().find(|&&i| {
                let e = &self.snap.entries[i];
                e.name[..] == sel.name[..] && (sel.dev_ino == (0, 0) || e.dev_ino == sel.dev_ino)
            });
            if let Some(&i) = found {
                self.selected = Some(Selection::of(&self.snap.entries[i]));
                return;
            }
        }
        if self.rows.is_empty() {
            self.selected = None;
            return;
        }
        let i = old_index.unwrap_or(0).min(self.rows.len() - 1);
        self.selected = Some(Selection::of(&self.snap.entries[self.rows[i]]));
    }

    fn select(&mut self, index: usize) {
        if let Some(&i) = self.rows.get(index) {
            self.selected = Some(Selection::of(&self.snap.entries[i]));
            self.sel_hint.set(index);
        }
    }

    fn move_by(&mut self, delta: isize) {
        if self.rows.is_empty() {
            return;
        }
        self.moved = true;
        let cur = self.selected_index().unwrap_or(0) as isize;
        let next = (cur + delta).clamp(0, self.rows.len() as isize - 1);
        self.select(next as usize);
    }

    fn save_cursor(&mut self) {
        let c = Cursor {
            selected: self.selected.clone(),
            scroll: self.scroll,
        };
        self.cursors.insert(self.snap.dir_path.clone(), c);
    }

    fn enter(&mut self, e: &EntryData) {
        self.save_cursor();
        let path = self.snap.dir_path.join(&*dfdu_tree::name_os(&e.name));
        let saved = self.cursors.get(&path).cloned();
        self.target = Target {
            node: Some(e.node),
            path,
            ident: Some(crate::snapshot::Ident {
                dev_ino: e.dev_ino,
                kind: e.kind,
            }),
        };
        self.moved = saved.is_some();
        self.selected = saved.as_ref().and_then(|c| c.selected.clone());
        self.scroll = saved.map_or(0, |c| c.scroll);
    }

    fn up(&mut self) {
        if self.snap.at_root {
            return;
        }
        self.save_cursor();
        let path = &self.snap.dir_path;
        let (Some(parent), Some(name)) = (path.parent(), path.file_name()) else {
            return;
        };
        let (parent, name) = (
            parent.to_path_buf(),
            dfdu_tree::name_bytes(name).into_owned(),
        );
        let saved = self.cursors.get(&parent).cloned();
        self.target = Target {
            node: None,
            path: parent,
            ident: None,
        };
        self.moved = true;
        self.scroll = saved.as_ref().map_or(0, |c| c.scroll);
        // Select the directory we came from, found again by name after the refresh.
        self.selected = Some(Selection {
            node: None,
            name,
            dev_ino: (0, 0),
        });
    }

    /// Opens the error list read by the run loop for [`Effect::ShowErrors`].
    pub fn show_errors(&mut self, list: ErrorList) {
        self.overlay = Overlay::Errors { list, cursor: 0 };
    }

    /// Goes to an entry below the scan root `root`: opens its parent directory and selects it.
    fn go_to(&mut self, path: &std::path::Path, root: &std::path::Path) {
        let (Some(parent), Some(name)) = (path.parent(), path.file_name()) else {
            return;
        };
        if path == root {
            return;
        }
        self.save_cursor();
        self.target = Target {
            node: None,
            path: parent.to_path_buf(),
            ident: None,
        };
        self.moved = true;
        self.scroll = 0;
        self.selected = Some(Selection {
            node: None,
            name: dfdu_tree::name_bytes(name).into_owned(),
            dev_ino: (0, 0),
        });
    }

    /// Handles one key. Returns effects for the run loop.
    fn set_typed(&mut self, t: String) {
        if let Overlay::ConfirmDelete { typed, .. } = &mut self.overlay {
            *typed = t;
        }
    }

    pub fn key(&mut self, k: KeyEvent, now: Instant) -> Vec<Effect> {
        let mut fx = Vec::new();
        if k.kind == KeyEventKind::Release {
            return fx;
        }
        self.last_input = now;
        if k.modifiers.contains(KeyModifiers::CONTROL) && k.code == KeyCode::Char('c') {
            self.quit = true;
            return fx;
        }
        match &self.overlay {
            Overlay::Help => {
                self.overlay = Overlay::None;
                return fx;
            }
            Overlay::Errors { list, cursor } => {
                let (n, page) = (list.items.len(), self.list_height.max(1));
                let cursor = *cursor;
                let next = match k.code {
                    KeyCode::Up | KeyCode::Char('k') => cursor.saturating_sub(1),
                    KeyCode::Down | KeyCode::Char('j') => cursor + 1,
                    KeyCode::PageUp => cursor.saturating_sub(page),
                    KeyCode::PageDown => cursor + page,
                    KeyCode::Home | KeyCode::Char('g') => 0,
                    KeyCode::End | KeyCode::Char('G') => n,
                    KeyCode::Enter | KeyCode::Right | KeyCode::Char('l') => {
                        if let Some(e) = list.items.get(cursor) {
                            let (path, root) = (e.path.clone(), list.root.clone());
                            self.overlay = Overlay::None;
                            self.go_to(&path, &root);
                        }
                        return fx;
                    }
                    _ => {
                        self.overlay = Overlay::None;
                        return fx;
                    }
                };
                if let Overlay::Errors { cursor, .. } = &mut self.overlay {
                    *cursor = next.min(n.saturating_sub(1));
                }
                return fx;
            }
            Overlay::ConfirmDelete {
                node, mode, typed, ..
            } => {
                let (node, mode) = (*node, *mode);
                let mut typed = typed.clone();
                let confirmed = match (mode, k.code) {
                    (_, KeyCode::Esc) => false,
                    (Mode::Trash, KeyCode::Char('y' | 'Y')) => true,
                    (Mode::Trash, _) => false,
                    (Mode::Permanent, KeyCode::Enter) => {
                        if typed != "delete" {
                            self.status = Some("not deleted: type delete to confirm".into());
                        }
                        typed == "delete"
                    }
                    (Mode::Permanent, KeyCode::Backspace) => {
                        typed.pop();
                        self.set_typed(typed);
                        return fx;
                    }
                    (Mode::Permanent, KeyCode::Char(c)) if typed.len() < 16 => {
                        typed.push(c);
                        self.set_typed(typed);
                        return fx;
                    }
                    (Mode::Permanent, _) => return fx,
                };
                if confirmed {
                    fx.push(Effect::Send(Command::Delete {
                        items: vec![node],
                        mode,
                    }));
                    self.status = Some(match mode {
                        Mode::Trash => "moving to the Trash…".into(),
                        Mode::Permanent => "deleting…".into(),
                    });
                }
                self.overlay = Overlay::None;
                return fx;
            }
            Overlay::None => {}
        }
        self.status = None;
        let page = self.list_height.max(1) as isize;
        match k.code {
            KeyCode::Char('q') => self.quit = true,
            KeyCode::Up | KeyCode::Char('k') => self.move_by(-1),
            KeyCode::Down | KeyCode::Char('j') => self.move_by(1),
            KeyCode::PageUp => self.move_by(-page),
            KeyCode::PageDown => self.move_by(page),
            KeyCode::Home | KeyCode::Char('g') => {
                self.moved = true;
                self.select(0);
            }
            KeyCode::End | KeyCode::Char('G') => {
                self.moved = true;
                self.select(self.rows.len().saturating_sub(1));
            }
            KeyCode::Right | KeyCode::Char('l') | KeyCode::Enter => {
                if let Some(e) = self.selected_entry().filter(|e| e.is_dir).cloned() {
                    self.enter(&e);
                }
            }
            KeyCode::Left | KeyCode::Char('h') | KeyCode::Backspace => self.up(),
            KeyCode::Char(c @ ('s' | 'n' | 'C' | 'M')) => {
                let key = match c {
                    's' => SortKey::Size,
                    'n' => SortKey::Name,
                    'C' => SortKey::Items,
                    _ => SortKey::Mtime,
                };
                self.sort = if self.sort.key == key {
                    SortSpec {
                        key,
                        desc: !self.sort.desc,
                    }
                } else {
                    SortSpec::for_key(key)
                };
                self.sort_requested = true;
            }
            KeyCode::Char('S') => self.sort_requested = true,
            KeyCode::Char(c @ ('a' | 'u')) => {
                let other = if c == 'a' {
                    SizeMode::Apparent
                } else {
                    SizeMode::Referenced
                };
                self.size_mode = if self.size_mode == other {
                    SizeMode::Disk
                } else {
                    other
                };
                self.sort_requested = true;
            }
            KeyCode::Char('e') => {
                self.hide_hidden = !self.hide_hidden;
                self.sort_requested = true;
            }
            KeyCode::Char('r') => {
                let node = match self.selected_entry() {
                    Some(e) if e.is_dir => Some(e.node),
                    _ => self.snap.dir_node,
                };
                if let Some(node) = node {
                    fx.push(Effect::Send(Command::Rescan { node }));
                    self.status = Some("rescan requested".into());
                }
            }
            KeyCode::Char('x') if self.snap.stalled > 0 => {
                fx.push(Effect::Send(Command::AbandonAllStalled));
                self.status = Some("skipping directories that are not responding".into());
            }
            KeyCode::Char(c @ ('d' | 'D')) => {
                if let Some(e) = self.selected_entry().filter(|e| !e.pseudo) {
                    self.overlay = Overlay::ConfirmDelete {
                        node: e.node,
                        name: String::from_utf8_lossy(&e.name).into_owned(),
                        size: e.size,
                        shared_out: e.shared_out,
                        is_dir: e.is_dir,
                        mode: if c == 'd' {
                            Mode::Trash
                        } else {
                            Mode::Permanent
                        },
                        typed: String::new(),
                        scanning: matches!(
                            e.mark,
                            crate::snapshot::Mark::Scanning
                                | crate::snapshot::Mark::Stalled
                                | crate::snapshot::Mark::Rescanning
                        ),
                    };
                }
            }
            KeyCode::Char('?') => self.overlay = Overlay::Help,
            KeyCode::Char('!') => fx.push(Effect::ShowErrors),
            KeyCode::Esc => self.overlay = Overlay::None,
            _ => {}
        }
        fx
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::snapshot::test_entry;

    fn snap(entries: Vec<EntryData>, version: u64) -> Snapshot {
        let root = test_entry(99, "/r", 0);
        Snapshot {
            dir_path: PathBuf::from("/r"),
            dir_node: Some(root.node),
            at_root: true,
            entries,
            busy: true,
            version,
            ..Default::default()
        }
    }

    fn key(c: KeyCode) -> KeyEvent {
        KeyEvent::new(c, KeyModifiers::NONE)
    }

    #[test]
    fn cursor_follows_node_across_resort_and_keeps_its_line() {
        let t0 = Instant::now();
        let mut app = App::new(Theme::no_color(), t0);
        let mut es: Vec<EntryData> = (0..10)
            .map(|i| test_entry(i, &format!("e{i}"), 1000 - i as u64 * 10))
            .collect();
        app.refresh(snap(es.clone(), 1), t0);
        app.list_height = 5;
        for _ in 0..3 {
            app.key(key(KeyCode::Down), t0);
        }
        app.scroll = 1;
        let picked = app.selected.clone().expect("selected").node;
        assert_eq!(app.selected_index(), Some(3));

        // e3 becomes the largest. Within the idle window nothing moves.
        es[3].size = 5000;
        app.refresh(snap(es.clone(), 2), t0 + Duration::from_millis(100));
        assert_eq!(app.selected_index(), Some(3));

        // After the idle delay the automatic sort moves e3 to the top; the cursor follows it
        // and the scroll offset keeps it on the same screen line where possible.
        app.refresh(snap(es, 3), t0 + Duration::from_secs(3));
        assert_eq!(app.selected.as_ref().and_then(|s| s.node), picked);
        assert_eq!(app.selected_index(), Some(0));
        assert_eq!(app.scroll, 0);
    }

    #[test]
    fn new_entries_wait_for_sort() {
        let t0 = Instant::now();
        let mut app = App::new(Theme::no_color(), t0);
        let mut es = vec![test_entry(0, "a", 10)];
        app.refresh(snap(es.clone(), 1), t0);
        es.push(test_entry(1, "b", 99));
        es.push(test_entry(2, "c", 50));
        app.key(key(KeyCode::Down), t0);
        app.refresh(snap(es.clone(), 2), t0);
        assert_eq!(app.rows.len(), 1);
        assert_eq!((app.new_count, app.new_largest), (2, 99));
        app.key(key(KeyCode::Char('S')), t0);
        app.refresh(snap(es, 3), t0);
        assert_eq!(app.rows.len(), 3);
        assert_eq!(app.new_count, 0);
    }

    #[test]
    fn finishing_scan_sorts_at_once_when_idle() {
        let t0 = Instant::now();
        let mut app = App::new(Theme::no_color(), t0);
        let mut es = vec![test_entry(0, "a", 10), test_entry(1, "b", 5)];
        app.refresh(snap(es.clone(), 1), t0);
        es[1].size = 999;
        let mut done = snap(es, 2);
        done.busy = false;
        // 100 ms after the first sort: well inside the minimum interval.
        app.refresh(done, t0 + Duration::from_millis(100));
        assert_eq!(app.snap.entries[app.rows[0]].name, b"b");
        // The cursor was never moved, so it goes to the top with the new order.
        assert_eq!(
            app.selected_entry().map(|e| e.name.to_vec()),
            Some(b"b".to_vec())
        );
    }

    #[test]
    fn rescanned_child_keeps_its_row() {
        let t0 = Instant::now();
        let mut app = App::new(Theme::no_color(), t0);
        let es = vec![
            test_entry(0, "a", 30),
            test_entry(1, "b", 20),
            test_entry(2, "c", 10),
        ];
        app.refresh(snap(es.clone(), 1), t0);
        app.key(key(KeyCode::Down), t0);
        let b = app.selected_entry().expect("b").clone();
        // A rescan of "b" swaps in a new node with the same name and identity.
        let b2 = EntryData {
            name: b.name.clone(),
            dev_ino: b.dev_ino,
            ..test_entry(7, "b", 25)
        };
        let new_node = b2.node;
        app.refresh(snap(vec![es[0].clone(), b2, es[2].clone()], 2), t0);
        assert_eq!(app.new_count, 0, "the swapped row is not shown as new");
        assert_eq!(app.rows.len(), 3);
        assert_eq!(app.selected_index(), Some(1));
        assert_eq!(app.selected.as_ref().and_then(|s| s.node), Some(new_node));
    }

    #[test]
    fn untouched_cursor_stays_at_top_after_resort() {
        let t0 = Instant::now();
        let mut app = App::new(Theme::no_color(), t0);
        let mut es = vec![test_entry(0, "a", 30), test_entry(1, "b", 20)];
        app.refresh(snap(es.clone(), 1), t0);
        es[1].size = 900;
        app.refresh(snap(es, 2), t0 + Duration::from_secs(3));
        assert_eq!(app.selected_index(), Some(0));
        assert_eq!(
            app.selected_entry().map(|e| e.name.to_vec()),
            Some(b"b".to_vec())
        );
        assert_eq!(app.scroll, 0);
    }

    #[test]
    fn sort_keys_apply_without_a_new_snapshot() {
        let t0 = Instant::now();
        let mut app = App::new(Theme::no_color(), t0);
        let es = vec![
            test_entry(0, "b", 30),
            test_entry(1, "c", 20),
            test_entry(2, "a", 10),
        ];
        app.refresh(snap(es, 1), t0);
        app.key(key(KeyCode::Down), t0);
        let picked = app.selected.as_ref().and_then(|s| s.node);
        app.key(key(KeyCode::Char('n')), t0);
        app.reorder(t0);
        let names: Vec<&[u8]> = app
            .rows
            .iter()
            .map(|&i| &app.snap.entries[i].name[..])
            .collect();
        assert_eq!(names, [b"a", b"b", b"c"]);
        assert_eq!(app.selected.as_ref().and_then(|s| s.node), picked);
        assert_eq!(app.selected_index(), Some(2));
    }

    #[test]
    fn trash_delete_sends_a_plan_after_y() {
        let t0 = Instant::now();
        let mut app = App::new(Theme::no_color(), t0);
        app.refresh(snap(vec![test_entry(0, "a", 10)], 1), t0);
        assert!(app.key(key(KeyCode::Char('d')), t0).is_empty());
        assert!(matches!(
            app.overlay,
            Overlay::ConfirmDelete {
                mode: Mode::Trash,
                ..
            }
        ));
        let fx = app.key(key(KeyCode::Char('y')), t0);
        assert!(matches!(
            fx.as_slice(),
            [Effect::Send(Command::Delete {
                mode: Mode::Trash,
                ..
            })]
        ));
        assert!(matches!(app.overlay, Overlay::None));
    }

    #[test]
    fn permanent_delete_needs_the_word_delete() {
        let t0 = Instant::now();
        let mut app = App::new(Theme::no_color(), t0);
        app.refresh(snap(vec![test_entry(0, "a", 10)], 1), t0);
        app.key(key(KeyCode::Char('D')), t0);
        // 'y' alone does nothing for a permanent delete.
        assert!(app.key(key(KeyCode::Char('y')), t0).is_empty());
        app.key(key(KeyCode::Backspace), t0);
        for c in "delet".chars() {
            assert!(app.key(key(KeyCode::Char(c)), t0).is_empty());
        }
        assert!(
            app.key(key(KeyCode::Enter), t0).is_empty(),
            "incomplete word must not delete"
        );
        assert!(matches!(app.overlay, Overlay::None));
        app.key(key(KeyCode::Char('D')), t0);
        for c in "delete".chars() {
            app.key(key(KeyCode::Char(c)), t0);
        }
        let fx = app.key(key(KeyCode::Enter), t0);
        assert!(matches!(
            fx.as_slice(),
            [Effect::Send(Command::Delete {
                mode: Mode::Permanent,
                ..
            })]
        ));
    }
}
