//! What the window keeps beside the session's state ([`ViewState`]), and what the user did in a
//! frame ([`Intent`]), carried out once it is drawn.

use std::time::Instant;

use dfdu_engine::{ErrorList, Mode};
use dfdu_session::actions::Action;
use dfdu_session::treemap::{ColorBy, TileTree, Toward};
use dfdu_session::type_filter::Row;
use dfdu_session::{cleanup, confirm, dupes, largest};
use dfdu_tree::HostPath;

use crate::goto::GoTo;
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
    /// A click on a breadcrumb above the scan root: a new scan of its directory.
    Reopen(HostPath),
    /// `Enter` in the go-to window, with the path typed.
    GoToFolder(String),
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
    /// The go-to window, while it is open.
    pub goto: Option<GoTo>,
    /// The names of the themes the palette offers, as the window lists them.
    pub themes: Vec<String>,
    /// The theme chosen in the palette, by its index in `themes`, for the window to apply.
    pub theme_pick: Option<usize>,
    /// The theme drawn with, by its index in `themes`, as the View menu ticks it.
    pub theme_current: Option<usize>,
    /// The Full disk access window was asked for, for the window to open.
    pub admin_open: bool,
    /// The Updates window was asked for, for the window to open.
    pub updates_open: bool,
    /// A scan of this path was asked for, outside the scan shown, for the window to open with
    /// the settings of this one.
    pub reopen: Option<HostPath>,
    /// The folders macOS keeps from the window that the scan could not read, as the banner
    /// on Full Disk Access names them, until it is dismissed (`access`).
    pub no_access: Option<Vec<String>>,
    /// The finished scan was checked for those folders.
    pub access_checked: bool,
}

impl ViewState {
    /// A window over the list takes the keys (the error list, the type picker, the help, the
    /// command palette, the go-to window), or the delete dialog of `s` is open.
    pub fn takes_keys(&self, s: &dfdu_session::SessionState) -> bool {
        self.errors.is_some()
            || self.types.is_some()
            || self.help.is_some()
            || self.palette.is_some()
            || self.goto.is_some()
            || s.confirm.is_some()
    }
}

impl Intent {
    /// Only an action can be the second `Parent` at the scan root: anything else the user did
    /// (a click, a filter edited) disarms the first.
    pub fn keeps_armed(&self) -> bool {
        matches!(self, Intent::Act(_))
    }
}

#[cfg(test)]
mod tests {
    use std::time::Instant;

    use dfdu_session::SessionState;

    use super::*;

    #[test]
    fn only_an_action_or_the_list_keeps_parent_armed() {
        assert!(Intent::Act(Action::Parent).keeps_armed());
        assert!(!Intent::Select(0).keeps_armed());
        assert!(!Intent::Filter("x".into()).keeps_armed());

        let s = SessionState::new(Instant::now());
        let mut view = ViewState::default();
        assert!(!view.takes_keys(&s));
        view.goto = Some(GoTo::default());
        assert!(view.takes_keys(&s));
    }
}
