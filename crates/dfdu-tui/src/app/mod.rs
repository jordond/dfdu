//! UI state and its update function. Keys turn into changes of [`App`] plus effects for the run
//! loop to carry out: [`App::key`] passes a key to the open overlay, the filter input or the
//! focused details pane, and otherwise looks it up in the action registry ([`keys`]).
//! [`App::refresh`] folds a new [`Snapshot`] into the state and applies the row-stability rules.
//! Each feature keeps its state in its own field and module (`marks`, `filter`, `details`,
//! `debug`, `motion`, `mouse`); what they read from the engine model arrives in
//! [`App::extras`].

pub mod keys;

use std::cell::Cell;
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::time::{Duration, Instant};

use dfdu_engine::Command;
use dfdu_tree::NodeRef;
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};

use crate::extras::Extras;
use crate::motion::Motion;
use crate::order::{Order, SortSpec};
use crate::problems::ErrorList;
use crate::snapshot::{EntryData, SizeMode, Snapshot, Target};
use crate::theme::{ColorLevel, Rgb, Theme};
use crate::view::Areas;
use crate::{confirm, debug, details, error_list, filter, help, marks, mouse, palette, treemap};

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
    Help(help::Help),
    /// What could not be read, with a cursor.
    Errors {
        list: ErrorList,
        cursor: usize,
    },
    ConfirmDelete(confirm::Confirm),
    Palette(palette::Palette),
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
    /// The terminal's colour level, which a theme chosen in the palette is resolved for. It
    /// can differ from the theme's own level: `mono` resolves without colours on any terminal.
    pub color_level: ColorLevel,
    /// The terminal's background colour, when it was queried.
    pub background: Option<Rgb>,
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
    /// Estimates asked for so far; the latest one's id.
    pub estimates: u64,
    pub started: Instant,
    pub done_at: Option<Instant>,
    pub quit: bool,
    pub marks: marks::Marks,
    pub filter: filter::Filter,
    pub details: details::Details,
    pub debug: debug::Debug,
    pub motion: Motion,
    pub mouse: mouse::Mouse,
    pub treemap: treemap::Treemap,
    pub cleanup: crate::cleanup::Cleanup,
    /// What the features read from the engine model with the last read.
    pub extras: Extras,
    /// Where the last frame drew each part of the screen.
    pub areas: Areas,
    /// The user's home directory, shown as `~` in the breadcrumbs.
    pub home: Option<PathBuf>,
    /// Counts the re-sorts of the rows; the treemap lays out again when it changes.
    pub sort_gen: u64,
    last_input: Instant,
    last_sort: Instant,
    sort_requested: bool,
    /// The filter changed: its rows are needed before the next frame.
    refilter_requested: bool,
    changed_since_sort: bool,
    /// The user moved the cursor in the current directory; re-sorts then follow the selected
    /// row instead of returning to the top.
    moved: bool,
    cursors: HashMap<PathBuf, Cursor>,
    /// Row where [`App::selected_index`] last found the selection.
    sel_hint: Cell<usize>,
}

/// Whether `e` gets a row when no filter hides it.
fn shown(hide_hidden: bool, e: &EntryData) -> bool {
    !e.ghost && !(hide_hidden && e.hidden())
}

/// One line describing a finished delete plan. On btrfs the freed space shows up in the free
/// space only after the transaction commits, and for snapshots after the cleaner ran.
pub fn delete_summary(d: &dfdu_engine::DeleteSummary) -> String {
    let done = match d.mode {
        Some(dfdu_engine::Mode::Trash) => "moved to the Trash",
        Some(dfdu_engine::Mode::Stage) => "staged",
        _ => "deleted",
    };
    let mut parts = vec![format!(
        "{done} {} ({})",
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
    if d.deleted > 0 && d.mode.is_some_and(|m| m.undoable()) {
        s.push_str("; U puts it back");
    }
    if d.btrfs {
        s.push_str("; btrfs frees the space when it commits (snapshots: in the background)");
    }
    s
}

impl App {
    /// An empty browser drawn with `theme`, on a terminal with the theme's colour level and
    /// background.
    pub fn new(theme: Theme, now: Instant) -> Self {
        App {
            color_level: theme.level(),
            background: theme.background(),
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
            estimates: 0,
            started: now,
            done_at: None,
            quit: false,
            marks: marks::Marks::default(),
            filter: filter::Filter::default(),
            details: details::Details::default(),
            debug: debug::Debug::default(),
            motion: Motion::default(),
            mouse: mouse::Mouse::default(),
            treemap: treemap::Treemap::default(),
            cleanup: Default::default(),
            extras: Extras::default(),
            areas: Areas::default(),
            home: None,
            sort_gen: 0,
            // No key pressed yet counts as idle.
            last_input: now.checked_sub(IDLE_BEFORE_SORT).unwrap_or(now),
            last_sort: now,
            sort_requested: true,
            refilter_requested: false,
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

    /// True when the features need a read of the engine model for the next frame although no
    /// new snapshot is due. A read must make it false: while it is true, the run loop reads
    /// again at once.
    pub fn extras_stale(&self) -> bool {
        marks::stale(self)
            || details::stale(self)
            || treemap::stale(self)
            || crate::cleanup::stale(self)
    }

    fn auto_sort_due(&self, now: Instant) -> bool {
        self.sort_pending()
            && now.duration_since(self.last_input) >= IDLE_BEFORE_SORT
            && now.duration_since(self.last_sort) >= MIN_SORT_INTERVAL
    }

    pub fn refresh(&mut self, mut snap: Snapshot, now: Instant) {
        self.motion.carry_ghosts(&self.snap, &mut snap, now);
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
        self.update_order(true, !appended, dir_changed, finished, old_index, now);
    }

    /// Applies sort requests and the automatic sort to the snapshot already held, for a frame
    /// drawn without reading the engine again.
    pub fn reorder(&mut self, now: Instant) {
        let old_index = self.selected_index();
        self.update_order(false, false, false, false, old_index, now);
    }

    /// Sorts or updates the row order, then puts the selection and scroll position back.
    /// `fresh`: a new snapshot was just folded in. `replaced`: its entries are not the previous
    /// ones with new ones appended (another directory, or a rescan replaced some), so indices
    /// into the previous entries mean nothing in the new ones.
    fn update_order(
        &mut self,
        fresh: bool,
        replaced: bool,
        dir_changed: bool,
        finished: bool,
        old_index: Option<usize>,
        now: Instant,
    ) {
        let requested = std::mem::take(&mut self.sort_requested);
        let refilter = std::mem::take(&mut self.refilter_requested);
        // `Some(automatic)` when the rows are sorted, `None` when new entries are synced.
        let sorted = if dir_changed || requested || refilter {
            Some(false)
        } else if self.auto_sort_due(now)
            || (finished && now.duration_since(self.last_input) >= IDLE_BEFORE_SORT)
        {
            Some(true)
        } else if fresh {
            None
        } else {
            // The entries and the order are those of the last frame: nothing below changes.
            return;
        };
        self.filter.prepare(&self.snap, replaced);
        let entries = &self.snap.entries;
        let (hide, filter) = (self.hide_hidden, &self.filter);
        let visible = |i: usize, e: &EntryData| shown(hide, e) && filter.matches(i, e);
        match sorted {
            // Only the filter changed: its rows are picked from the kept exact sort of every
            // entry it can show, in the order an exact sort of only those gives.
            Some(false)
                if refilter
                    && !requested
                    && !fresh
                    && self.order.refilter(entries, visible, self.sort) => {}
            Some(false) => {
                self.order = Order::default();
                if filter.active() {
                    self.order.sort_exact(entries, visible, self.sort);
                } else {
                    self.order
                        .sort_exact_unfiltered(entries, visible, self.sort);
                }
            }
            Some(true) => self.order.sort_auto(entries, visible, self.sort),
            None => self.order.sync(entries, visible, self.sort),
        }
        let resorted = sorted.is_some();
        if resorted {
            self.sort_gen += 1;
            self.last_sort = now;
            self.changed_since_sort = false;
        }

        // The rows before a re-sort go to the motion hook; otherwise the buffer is reused.
        let old_rows = if resorted && !dir_changed {
            std::mem::take(&mut self.rows)
        } else {
            self.rows.clear();
            Vec::new()
        };
        self.rows.extend_from_slice(self.order.rows());
        if let Some(auto) = sorted
            && !dir_changed
        {
            self.motion
                .resorted(&old_rows, &self.rows, entries, auto, now);
        }
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

    /// Asks for an exact sort before the next frame.
    pub(crate) fn request_sort(&mut self) {
        self.sort_requested = true;
    }

    /// Asks for the rows of a changed filter before the next frame: those an exact sort gives.
    pub(crate) fn request_refilter(&mut self) {
        self.refilter_requested = true;
    }

    /// Makes the order keep an exact sort of every entry the filter can show, unless it holds
    /// one, so the keystrokes that change the filter only pick their rows from it.
    pub(crate) fn keep_exact_order(&mut self) {
        let hide = self.hide_hidden;
        self.order
            .keep_exact(&self.snap.entries, |_, e| shown(hide, e), self.sort);
    }

    /// Records user input at `now`: automatic re-sorts wait until the user is idle.
    pub(crate) fn note_input(&mut self, now: Instant) {
        self.last_input = now;
    }

    pub(crate) fn select(&mut self, index: usize) {
        if let Some(&i) = self.rows.get(index) {
            self.selected = Some(Selection::of(&self.snap.entries[i]));
            self.sel_hint.set(index);
        }
    }

    /// Selects row `index` as a cursor move of the user's, so re-sorts follow the selected row.
    pub(crate) fn move_to(&mut self, index: usize) {
        self.moved = true;
        self.select(index);
    }

    pub(crate) fn move_by(&mut self, delta: isize) {
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
    pub(crate) fn go_to(&mut self, path: &std::path::Path, root: &std::path::Path) {
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

    /// Handles a key: the open overlay takes it first, then the filter input while it is
    /// edited, then the details pane while it has the focus, then the treemap's moves between
    /// tiles, and otherwise the action bound to it in `keys::ACTIONS`.
    pub fn key(&mut self, k: KeyEvent, now: Instant) -> Vec<Effect> {
        if k.kind == KeyEventKind::Release {
            return Vec::new();
        }
        self.note_input(now);
        if k.modifiers.contains(KeyModifiers::CONTROL) && k.code == KeyCode::Char('c') {
            self.quit = true;
            return Vec::new();
        }
        match self.overlay {
            Overlay::None => {}
            Overlay::Help(_) => return help::key(self, k),
            Overlay::Errors { .. } => return error_list::key(self, k),
            Overlay::ConfirmDelete { .. } => return confirm::key(self, k),
            Overlay::Palette { .. } => return palette::key(self, k, now),
        }
        if filter::editing(self) {
            return filter::key(self, k, now);
        }
        if details::focused(self)
            && let Some(fx) = details::key(self, &k)
        {
            return fx;
        }
        if crate::cleanup::active(self)
            && let Some(fx) = crate::cleanup::key(self, &k)
        {
            return fx;
        }
        if treemap::active(self)
            && let Some(fx) = treemap::key(self, &k)
        {
            return fx;
        }
        self.status = None;
        match keys::action_for(&k) {
            Some(a) => keys::run(self, a, now),
            None => Vec::new(),
        }
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::snapshot::test_entry;
    use crate::tests::key;

    pub(crate) fn snap(entries: Vec<EntryData>, version: u64) -> Snapshot {
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
}
