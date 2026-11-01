//! What the window keeps beside the session's state ([`ViewState`]), and what the user did in a
//! frame ([`Intent`]), carried out once it is drawn.

use std::time::Instant;

use dfdu_engine::{ErrorList, Mode};
use dfdu_session::actions::Action;
use dfdu_session::treemap::{ColorBy, TileTree, Toward};
use dfdu_session::type_filter::Row;
use dfdu_session::{cleanup, confirm, dupes, largest};
use dfdu_tree::HostPath;

use crate::help::Help;
use crate::palette::Palette;

/// What the user did in a frame, carried out once it is drawn.
#[derive(Clone, Debug, PartialEq)]
pub enum Intent {
    Act(Action),
    /// A click on a row.
    Select(usize),
    /// A double click on a row.
    Open(usize),
    /// A click on the disclosure triangle of a row in tree mode.
    ToggleRow(usize),
    /// A click on the mark of a row: marks or unmarks it, the selection staying on it.
    Mark(usize),
    /// A click on a breadcrumb: its directory opens with `child`, the one below it on the way
    /// to the current directory, selected.
    GoTo {
        child: HostPath,
    },
    /// The filter's text changed.
    Filter(String),
    /// `Enter` in the filter.
    FilterSubmit,
    /// `Esc` in the filter.
    FilterCancel,
    /// Input of the delete dialog.
    Confirm(confirm::Intent),
    /// The delete dialog's method chosen.
    Method(Mode),
    /// The word typed in the delete dialog changed.
    Typed(String),
    /// A row of the type picker chosen or unchosen.
    Type(Row),
    /// A click on an entry of the error list: shows it in the list.
    ShowError(usize),
    /// An arrow on the map: the selection moves to the nearest tile that way.
    Toward(Toward),
    /// A click on top-level tile `top` of the map: selects it, and a double click opens it.
    Tile {
        top: usize,
        open: bool,
    },
    /// The map's colouring chosen.
    MapColor(ColorBy),
    /// A click in the Largest view.
    Largest(largest::Intent),
    /// A click or a key in the Cleanup view.
    Cleanup(cleanup::Intent),
    /// A click or a key in the native command pane.
    Pane(cleanup::pane::Intent),
    /// A click or a key in the Duplicates view.
    Dupes(dupes::Intent),
    /// The comparison of duplicates that runs is stopped.
    DupesCancel,
    /// Theme `i` of [`ViewState::themes`] chosen in the palette.
    Theme(usize),
    /// The banner's button that opens the Full Disk Access settings (`access`).
    OpenAccessSettings,
    /// The banner on Full Disk Access dismissed.
    DismissAccess,
}

/// What the window keeps beside the session's state.
#[derive(Debug, Default)]
pub struct ViewState {
    /// The filter field's text while it is edited.
    pub filter_text: String,
    /// Give the filter field the keyboard in the next frame.
    pub focus_filter: bool,
    /// Scroll the list to the selected row in the next frame.
    pub follow: bool,
    /// The word typed in the delete dialog, as its field shows it.
    pub typed: String,
    /// The estimate of the delete dialog drawn last: a dialog with another one is new, and its
    /// Cancel button gets the keyboard.
    pub dialog: Option<u64>,
    /// The error list, while its window is open.
    pub errors: Option<ErrorList>,
    /// The type picker's search, while the picker is open.
    pub types: Option<String>,
    /// The undo offered, and when it was first seen: a staged delete is put back only for a
    /// while.
    pub undo_seen: Option<(u64, Instant)>,
    /// The map's tiles as the last frame laid them out.
    pub map: Option<TileTree>,
    /// The command palette, while it is open.
    pub palette: Option<Palette>,
    /// The help, while it is open.
    pub help: Option<Help>,
    /// The names of the themes the palette offers, as the window lists them.
    pub themes: Vec<String>,
    /// The theme chosen in the palette, by its index in `themes`, for the window to apply.
    pub theme_pick: Option<usize>,
    /// The folders macOS keeps from the window that the scan could not read, as the banner
    /// on Full Disk Access names them, until it is dismissed (`access`).
    pub no_access: Option<Vec<String>>,
    /// The finished scan was checked for those folders.
    pub access_checked: bool,
}
