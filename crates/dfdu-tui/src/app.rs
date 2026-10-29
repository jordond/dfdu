//! UI state and its update function. Keys turn into changes of [`App`] plus effects for the run
//! loop to carry out: [`App::key`] passes a key to the open overlay, the filter input or the
//! focused details pane, and otherwise looks it up in the action registry ([`keys`]).
//! [`App::refresh`] folds a new [`Snapshot`] into the state and applies the row-stability rules.
//! [`views`] switches between the five views.
//! Each feature keeps its state in its own field and module (`marks`, `filter`, `details`,
//! `debug`, `motion`, `mouse`); what they read from the engine model arrives in
//! [`App::extras`].

pub mod keys;
mod refresh;
pub mod views;

use std::cell::Cell;
use std::collections::HashMap;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use dfdu_engine::{Command, ErrorList, SizeMode};
use dfdu_tree::{HostPath, NodeRef};
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};

use crate::features::{debug, details, treemap};
use crate::input::mouse;
use crate::model::extras::Extras;
use crate::model::snapshot::{EntryData, Snapshot, Target};
use crate::overlay::{confirm, error_list, help, mark_pattern, palette, type_filter};
use crate::state::motion::Motion;
use crate::state::order::{Order, SortSpec};
use crate::state::{filter, marks};
use crate::theme::{ColorLevel, Rgb, Theme};
use crate::view::Areas;

/// Automatic re-sort after this long without input...
pub const IDLE_BEFORE_SORT: Duration = Duration::from_millis(1500);
/// ...and at most this often.
pub const MIN_SORT_INTERVAL: Duration = Duration::from_secs(2);

#[derive(Debug)]
pub enum Effect {
    Send(Command),
    /// Read the error list from the engine and pass it to [`App::show_errors`].
    ShowErrors,
    /// Read the duplicate candidates and start comparing them (`dupes::begin`).
    FindDupes,
    /// Put the text on the clipboard through the terminal (`selected_path::copy`).
    Copy(String),
    /// Show the path in the file manager, without waiting for it (`selected_path::open`).
    Open {
        path: PathBuf,
        is_dir: bool,
    },
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
    /// The file type picker.
    Types(type_filter::TypePicker),
    /// The glob that marks the names it matches.
    MarkPattern(mark_pattern::Prompt),
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

#[expect(
    clippy::struct_excessive_bools,
    reason = "independent flags of the browser state, not one state spread over several bools"
)]
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
    /// The optional columns of the list (`c`).
    pub columns: crate::view::columns::Columns,
    /// Tree mode: the expanded directories and the fold threshold. `None` when off.
    pub tree: Option<crate::state::tree::TreeState>,
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
    /// Rows the last frame had room for; zero before the first frame, when no rows are on screen
    /// to keep sorted ([`App::refresh`]).
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
    /// The file type filter (`overlay::type_filter`); empty when off.
    pub types: dfdu_tree::filetype::TypeFilter,
    pub details: details::Details,
    /// The marks pane, in the place of the details pane while it is open.
    pub marks_pane: crate::features::marks_pane::MarksPane,
    pub debug: debug::Debug,
    pub motion: Motion,
    pub mouse: mouse::Mouse,
    pub treemap: treemap::Treemap,
    pub cleanup: crate::features::cleanup::Cleanup,
    pub dupes: crate::features::dupes::Dupes,
    pub largest: crate::features::largest::Largest,
    /// What the features read from the engine model with the last read.
    pub extras: Extras,
    /// Where the last frame drew each part of the screen.
    pub areas: Areas,
    /// The user's home directory, shown as `~` in the breadcrumbs; `None` for a scan that is
    /// not of this machine.
    pub home: Option<PathBuf>,
    /// The sizes of the last recorded scan of this root, to show what grew since.
    pub growth: Option<std::sync::Arc<dfdu_store::history::Growth>>,
    /// Mounts the scan did not enter and links out of it, as looked up (`outside`).
    pub outside: crate::model::outside::Outside,
    /// Counts the re-sorts of the rows; the treemap lays out again when it changes.
    pub sort_gen: u64,
    /// Nothing is deleted: the actions that delete are left out (`keys::Action::deletes`) and
    /// cleanup commands run only as dry runs.
    pub read_only: bool,
    last_input: Instant,
    last_sort: Instant,
    sort_requested: bool,
    /// The filter changed: its rows are needed before the next frame.
    refilter_requested: bool,
    changed_since_sort: bool,
    /// The user moved the cursor in the current directory; re-sorts then follow the selected
    /// row instead of returning to the top.
    moved: bool,
    cursors: HashMap<HostPath, Cursor>,
    /// Row where [`App::selected_index`] last found the selection.
    sel_hint: Cell<usize>,
}

/// Whether `e` gets a row when no filter hides it.
fn shown(hide_hidden: bool, e: &EntryData) -> bool {
    !e.ghost && (!hide_hidden || !e.hidden())
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
    if d.deleted > 0 && d.mode.is_some_and(dfdu_engine::Mode::undoable) {
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
    #[must_use]
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
            columns: crate::view::columns::Columns::default(),
            tree: None,
            hide_hidden: false,
            selected: None,
            rows: Vec::new(),
            new_count: 0,
            new_largest: 0,
            rows_largest: 0,
            scroll: 0,
            list_height: 0,
            overlay: Overlay::None,
            status: None,
            estimates: 0,
            started: now,
            done_at: None,
            quit: false,
            marks: marks::Marks::default(),
            filter: filter::Filter::default(),
            types: dfdu_tree::filetype::TypeFilter::default(),
            details: details::Details::default(),
            marks_pane: crate::features::marks_pane::MarksPane::default(),
            debug: debug::Debug::default(),
            motion: Motion::default(),
            mouse: mouse::Mouse::default(),
            treemap: treemap::Treemap::default(),
            cleanup: crate::features::cleanup::Cleanup::default(),
            dupes: crate::features::dupes::Dupes::default(),
            largest: crate::features::largest::Largest::default(),
            extras: Extras::default(),
            areas: Areas::default(),
            home: None,
            growth: None,
            outside: crate::model::outside::Outside::default(),
            sort_gen: 0,
            read_only: false,
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
            || crate::features::cleanup::stale(self)
            || crate::features::dupes::stale(self)
            || crate::features::largest::stale(self)
    }

    /// The tree mode state while the rows are a tree: tree mode is on and the treemap, whose
    /// tiles are the rows of one directory, is off.
    pub fn tree_shown(&self) -> Option<&crate::state::tree::TreeState> {
        self.tree.as_ref().filter(|_| !self.treemap.on)
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
        let cur = self.selected_index().unwrap_or(0);
        let next = cur.saturating_add_signed(delta).min(self.rows.len() - 1);
        self.select(next);
    }

    fn save_cursor(&mut self) {
        let c = Cursor {
            selected: self.selected.clone(),
            scroll: self.scroll,
        };
        self.cursors.insert(self.snap.dir_path.clone(), c);
    }

    /// Opens the directory of entry `index` of the snapshot: in tree mode a row at any depth,
    /// and the expanded directories stay expanded.
    fn enter(&mut self, index: usize) {
        self.save_cursor();
        let target = crate::state::tree::target_of(&self.snap, index);
        let saved = self.cursors.get(&target.path).cloned();
        self.target = target;
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
        let name = name.to_vec();
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
    pub(crate) fn go_to(&mut self, path: &HostPath, root: &HostPath) {
        let (Some(parent), Some(name)) = (path.parent(), path.file_name()) else {
            return;
        };
        if path == root {
            return;
        }
        self.save_cursor();
        self.target = Target {
            node: None,
            path: parent,
            ident: None,
        };
        self.moved = true;
        self.scroll = 0;
        self.selected = Some(Selection {
            node: None,
            name: name.to_vec(),
            dev_ino: (0, 0),
        });
    }

    /// Handles a key: the open overlay takes it first, then the filter input while it is
    /// edited, then the marks pane while it is open, then the details pane while it has the focus, then the treemap's moves between
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
            Overlay::Types(_) => return type_filter::key(self, k),
            Overlay::MarkPattern(_) => return mark_pattern::key(self, &k),
        }
        if filter::editing(self) {
            return filter::key(self, k, now);
        }
        if crate::features::marks_pane::shown(self)
            && let Some(fx) = crate::features::marks_pane::key(self, &k, now)
        {
            return fx;
        }
        if details::focused(self)
            && let Some(fx) = details::key(self, &k)
        {
            return fx;
        }
        if crate::features::largest::active(self)
            && let Some(fx) = crate::features::largest::key(self, &k)
        {
            return fx;
        }
        if crate::features::dupes::active(self)
            && let Some(fx) = crate::features::dupes::key(self, &k)
        {
            return fx;
        }
        if crate::features::cleanup::active(self)
            && let Some(fx) = crate::features::cleanup::key(self, &k)
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
mod tests;
