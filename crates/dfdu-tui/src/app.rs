//! UI state and its update function. [`App`] holds the browser's state shared with other front
//! ends ([`App::session`], a [`SessionState`]) and what only the terminal shows. Keys turn into
//! changes of it plus effects for the run loop to carry out: [`App::key`] passes a key to the
//! open overlay, the filter input or the focused details pane, and otherwise looks it up in the
//! action registry ([`keys`]). [`App::refresh`] folds a new snapshot into the state and applies
//! the row-stability rules. [`views`] switches between the five views.
//! Each feature keeps its state in its own field and module (`list`, `marks` and `filter` in
//! the session; `details`, `debug`, `motion`, `mouse`); what they read from the engine model
//! arrives in [`App::extras`].

mod cursor;
pub(crate) mod keys;
mod refresh;
pub(crate) mod views;

use std::collections::HashMap;
use std::time::Instant;

use dfdu_engine::ErrorList;
use dfdu_session::SessionState;
use dfdu_tree::HostPath;
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};

use crate::features::{debug, details, treemap};
use crate::input::{line_keys, mouse};
use crate::model::extras::Extras;

use crate::overlay::{confirm, error_list, help, mark_pattern, palette, type_filter};
use crate::state::filter;
use crate::state::motion::Motion;
use crate::theme::{ColorLevel, Rgb, Theme};
use crate::view::Areas;
pub use dfdu_session::state::{IDLE_BEFORE_SORT, MIN_SORT_INTERVAL, Selection};

pub use dfdu_session::effect::Effect;

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

pub struct App {
    pub theme: Theme,
    /// The terminal's colour level, which a theme chosen in the palette is resolved for. It
    /// can differ from the theme's own level: `mono` resolves without colours on any terminal.
    pub color_level: ColorLevel,
    /// The terminal's background colour, when it was queried.
    pub background: Option<Rgb>,
    /// What the browser holds besides how the terminal shows it: the directory, its rows and
    /// selection, sort, marks, filters and status line.
    pub session: SessionState,
    /// Where the Files list is scrolled to: its first row on screen.
    pub scroll: usize,
    /// The scroll of each directory left, restored on the way back as the session restores
    /// its selection.
    scrolls: HashMap<HostPath, usize>,
    /// The optional columns of the list (`c`).
    pub columns: crate::view::columns::Columns,
    /// Rows the last frame had room for in the view shown; zero before the first frame, when no
    /// rows are on screen to keep sorted ([`App::refresh`]).
    pub list_height: usize,
    pub overlay: Overlay,
    pub quit: bool,
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
            session: SessionState::new(now),
            scroll: 0,
            scrolls: HashMap::new(),
            columns: crate::view::columns::Columns::default(),
            list_height: 0,
            overlay: Overlay::None,
            quit: false,
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
            ..App::new(ui.theme.clone(), now)
        };
        let s = &mut app.session;
        s.home.clone_from(&ui.home);
        s.types = ui.types.clone();
        s.read_only = ui.read_only;
        s.outside.local = ui.local;
        app
    }

    /// True when the features need a read of the engine model for the next frame although no
    /// new snapshot is due. A read must make it false: while it is true, the run loop reads
    /// again at once.
    pub fn extras_stale(&self) -> bool {
        dfdu_session::extras::stale(&self.session)
            || details::stale(self)
            || crate::features::cleanup::stale(self)
            || crate::features::dupes::stale(self)
            || crate::features::largest::stale(self)
    }

    /// The tree mode state while the rows are a tree ([`SessionState::tree_shown`]).
    pub fn tree_shown(&self) -> Option<&crate::state::tree::TreeState> {
        self.session.tree_shown()
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
        self.session.note_input(now);
        if k.modifiers.contains(KeyModifiers::CONTROL) && k.code == KeyCode::Char('c') {
            self.quit = true;
            return Vec::new();
        }
        match self.overlay {
            Overlay::None => {}
            Overlay::Help(_) => return help::key(self, k),
            Overlay::Errors { .. } => return error_list::key(self, k),
            Overlay::ConfirmDelete { .. } => return confirm::key(self, k),
            Overlay::Palette { .. } => return palette::key(self, k),
            Overlay::Types(_) => return type_filter::key(self, k),
            Overlay::MarkPattern(_) => return mark_pattern::key(self, k),
        }
        if filter::editing(self) {
            // `↑`/`↓` move the cursor in the list; the other keys edit the query.
            match k.code {
                KeyCode::Up => self.session.move_by(-1),
                KeyCode::Down => self.session.move_by(1),
                _ => {
                    if let Some(op) = line_keys::op(&k) {
                        return filter::edit(self, op, now);
                    }
                }
            }
            return Vec::new();
        }
        if crate::features::marks_pane::is_open(self)
            && let Some(fx) = crate::features::marks_pane::key(self, &k)
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
        self.session.status = None;
        match keys::action_for(&k) {
            Some(a) => keys::run(self, a),
            None => Vec::new(),
        }
    }
}

#[cfg(test)]
mod tests;
