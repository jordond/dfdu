//! UI state and its update function. Keys turn into changes of [`App`] plus effects for the run
//! loop to carry out: [`App::key`] passes a key to the open overlay, the filter input or the
//! focused details pane, and otherwise looks it up in the action registry ([`keys`]).
//! [`App::refresh`] folds a new [`Snapshot`] into the state and applies the row-stability rules.
//! [`views`] switches between the five views.
//! Each feature keeps its state in its own field and module (`marks`, `filter`, `details`,
//! `debug`, `motion`, `mouse`); what they read from the engine model arrives in
//! [`App::extras`].

mod cursor;
pub mod keys;
mod refresh;
pub mod views;

use std::cell::Cell;
use std::collections::HashMap;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use dfdu_engine::{Command, ErrorList, SizeMode};
use dfdu_tree::HostPath;
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
use cursor::Cursor;
pub use cursor::Selection;

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
    /// The view shown (`1`-`5`); `views::switch` changes it.
    pub view: views::View,
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
    /// The scan shown is the cached one, being read or brought up to date.
    pub cached: Option<crate::model::cache_link::Cached>,
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
            view: views::View::default(),
            treemap: treemap::Treemap::default(),
            cleanup: crate::features::cleanup::Cleanup::default(),
            dupes: crate::features::dupes::Dupes::default(),
            largest: crate::features::largest::Largest::default(),
            extras: Extras::default(),
            areas: Areas::default(),
            home: None,
            growth: None,
            cached: None,
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

    /// The browser `ui` describes: its theme, colour level and background, `~`, starting file
    /// type filter, whether this machine is looked up and whether anything can be deleted. The
    /// run loop keeps the rest (mouse, debug panel, history, cache).
    #[must_use]
    pub fn with_options(ui: &crate::UiOptions, now: Instant) -> Self {
        let mut app = App {
            color_level: ui.color_level,
            background: ui.background,
            home: ui.home.clone(),
            types: ui.types.clone(),
            read_only: ui.read_only,
            ..App::new(ui.theme.clone(), now)
        };
        app.outside.local = ui.local;
        app
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
        self.tree.as_ref().filter(|_| self.view != views::View::Map)
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

    /// Opens the error list read by the run loop for [`Effect::ShowErrors`].
    pub fn show_errors(&mut self, list: ErrorList) {
        self.overlay = Overlay::Errors { list, cursor: 0 };
    }

    /// Handles a key: the open overlay takes it first, then the filter input while it is
    /// edited, then the marks pane while it is open, then the details pane while it has the
    /// focus, then the view shown (the treemap's moves between tiles once it is drawn), and
    /// otherwise the action bound to it in `keys::ACTIONS`.
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
            Overlay::MarkPattern(_) => return mark_pattern::key(self, k),
        }
        if filter::editing(self) {
            return filter::key(self, k, now);
        }
        if crate::features::marks_pane::is_open(self)
            && let Some(fx) = crate::features::marks_pane::key(self, &k, now)
        {
            return fx;
        }
        if details::focused(self)
            && let Some(fx) = details::key(self, &k)
        {
            return fx;
        }
        let fx = match views::current(self) {
            views::View::Largest => crate::features::largest::key(self, &k),
            views::View::Duplicates => crate::features::dupes::key(self, &k),
            views::View::Cleanup => crate::features::cleanup::key(self, &k),
            views::View::Map if treemap::active(self) => treemap::key(self, &k),
            views::View::Map | views::View::Files => None,
        };
        if let Some(fx) = fx {
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
