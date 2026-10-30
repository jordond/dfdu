//! The action registry: every user action once, with its keys, label and group. Key handling,
//! the help overlay and the command palette all read [`ACTIONS`], so a binding shown anywhere is
//! the binding that works.

mod act;
mod lookup;

pub use act::run;
#[cfg(test)]
pub(crate) use lookup::parse;
pub use lookup::{action_for, cursor_after, first_key, half_page, offered};

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Action {
    Quit,
    Up,
    Down,
    PageUp,
    PageDown,
    HalfPageUp,
    HalfPageDown,
    First,
    Last,
    Open,
    Enter,
    Parent,
    SortSize,
    SortName,
    SortItems,
    SortMtime,
    Resort,
    ToggleApparent,
    ToggleReferenced,
    ToggleHidden,
    Columns,
    ToggleTree,
    ExpandChain,
    CycleFold,
    Rescan,
    EnterMount,
    SkipStalled,
    Trash,
    DeletePermanent,
    Undo,
    Help,
    Errors,
    Close,
    ToggleMark,
    ClearMarks,
    RangeMark,
    MarkPattern,
    InvertMarks,
    CopyPath,
    OpenPath,
    Filter,
    Types,
    Details,
    FocusDetails,
    Palette,
    Debug,
    ViewFiles,
    ViewMap,
    ViewCleanup,
    ViewDupes,
    ViewLargest,
    PrevView,
    NextView,
    TickSuggested,
    DupesKeep,
    CompareAgain,
    CleanupFilter,
    CleanupCommand,
    TreemapDeeper,
    TreemapShallower,
    TreemapColor,
}

impl Action {
    /// Deletes, or puts back what was deleted: left out of a read-only browser.
    pub fn deletes(self) -> bool {
        matches!(self, Action::Trash | Action::DeletePermanent | Action::Undo)
    }

    /// Moves the cursor of the list: a range mark ([`marks::range`]) marks the rows it passes.
    pub fn moves(self) -> bool {
        matches!(
            self,
            Action::Up
                | Action::Down
                | Action::PageUp
                | Action::PageDown
                | Action::HalfPageUp
                | Action::HalfPageDown
                | Action::First
                | Action::Last
        )
    }
}

/// The status line after a key that deletes, in a read-only browser.
pub const READ_ONLY: &str = "read-only: nothing is deleted";

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Group {
    Navigate,
    Sort,
    Display,
    Act,
    Panels,
    Global,
}

#[derive(Copy, Clone, Debug)]
pub struct ActionInfo {
    pub action: Action,
    /// The keys that run it, separated by spaces: a single character, a name (`↑`, `PgUp`,
    /// `Enter`, `Space`, `F1`) or `Ctrl-` and a letter. Empty for actions without a key.
    pub keys: &'static str,
    pub label: &'static str,
    pub group: Group,
}

const fn row(action: Action, keys: &'static str, label: &'static str, group: Group) -> ActionInfo {
    ActionInfo {
        action,
        keys,
        label,
        group,
    }
}

pub const ACTIONS: &[ActionInfo] = &[
    row(Action::Up, "↑ k", "move up", Group::Navigate),
    row(Action::Down, "↓ j", "move down", Group::Navigate),
    row(Action::PageUp, "PgUp", "page up", Group::Navigate),
    row(Action::PageDown, "PgDn", "page down", Group::Navigate),
    row(
        Action::HalfPageUp,
        "Ctrl-U",
        "half a page up",
        Group::Navigate,
    ),
    row(
        Action::HalfPageDown,
        "Ctrl-D",
        "half a page down",
        Group::Navigate,
    ),
    row(Action::First, "g Home", "first entry", Group::Navigate),
    row(Action::Last, "G End", "last entry", Group::Navigate),
    row(
        Action::Open,
        "→ l",
        "open directory (tree mode: expand it)",
        Group::Navigate,
    ),
    row(
        Action::Enter,
        "Enter",
        "open directory, even in tree mode",
        Group::Navigate,
    ),
    row(
        Action::Parent,
        "← h Backspace <",
        "parent directory (tree mode: collapse)",
        Group::Navigate,
    ),
    row(
        Action::SortSize,
        "s",
        "sort by size (again: reverse)",
        Group::Sort,
    ),
    row(
        Action::SortName,
        "n",
        "sort by name (again: reverse)",
        Group::Sort,
    ),
    row(
        Action::SortItems,
        "C",
        "sort by items (again: reverse)",
        Group::Sort,
    ),
    row(
        Action::SortMtime,
        "M",
        "sort by modification time (again: reverse)",
        Group::Sort,
    ),
    row(Action::Resort, "S", "re-sort now", Group::Sort),
    row(
        Action::ViewFiles,
        "1",
        "Files view: the entries of this directory",
        Group::Display,
    ),
    row(
        Action::ViewMap,
        "2",
        "Map view: this directory as a treemap",
        Group::Display,
    ),
    row(
        Action::ViewCleanup,
        "3",
        "Cleanup view: what the rules found in the whole scan",
        Group::Display,
    ),
    row(
        Action::ViewDupes,
        "4",
        "Duplicates view: identical files in the whole scan",
        Group::Display,
    ),
    row(
        Action::ViewLargest,
        "5",
        "Largest view: the largest files below this directory",
        Group::Display,
    ),
    row(Action::PrevView, "[", "previous view", Group::Display),
    row(Action::NextView, "]", "next view", Group::Display),
    row(
        Action::DupesKeep,
        "K",
        "duplicates: change which copy is kept",
        Group::Display,
    ),
    row(
        Action::CleanupFilter,
        "f",
        "cleanup: only the safer levels, one fewer each time",
        Group::Display,
    ),
    row(
        Action::TreemapDeeper,
        "+",
        "map: one level deeper",
        Group::Display,
    ),
    row(
        Action::TreemapShallower,
        "-",
        "map: one level less",
        Group::Display,
    ),
    row(
        Action::TreemapColor,
        "p",
        "map: colour by category, age or top folder",
        Group::Display,
    ),
    row(
        Action::ToggleApparent,
        "a",
        "toggle apparent size / disk usage",
        Group::Display,
    ),
    row(
        Action::ToggleReferenced,
        "u",
        "toggle disk usage like du (shared data once per file)",
        Group::Display,
    ),
    row(
        Action::ToggleHidden,
        "e",
        "hide or show hidden files",
        Group::Display,
    ),
    row(
        Action::Columns,
        "c",
        "list columns: items, then items and modified, then neither",
        Group::Display,
    ),
    row(
        Action::ToggleTree,
        "T",
        "tree mode: expand directories in place",
        Group::Display,
    ),
    row(
        Action::ExpandChain,
        "*",
        "tree mode: expand the largest, level by level",
        Group::Display,
    ),
    row(
        Action::CycleFold,
        "z",
        "tree mode: fold under 1%, then 0.1%, then none",
        Group::Display,
    ),
    row(
        Action::Rescan,
        "r",
        "rescan the selected directory",
        Group::Act,
    ),
    row(
        Action::EnterMount,
        "m",
        "scan into the selected mount (another filesystem)",
        Group::Act,
    ),
    row(
        Action::SkipStalled,
        "x",
        "skip directories that are not responding",
        Group::Act,
    ),
    row(
        Action::ToggleMark,
        "Space",
        "mark or unmark, then move down",
        Group::Act,
    ),
    row(
        Action::ClearMarks,
        "X",
        "untick all: clear every mark",
        Group::Act,
    ),
    row(
        Action::TickSuggested,
        "A",
        "tick what Cleanup or Duplicates suggests deleting",
        Group::Act,
    ),
    row(
        Action::CompareAgain,
        "R",
        "duplicates: compare the files again",
        Group::Act,
    ),
    row(
        Action::CleanupCommand,
        "N",
        "cleanup: the item's own cleanup command (dry run first)",
        Group::Act,
    ),
    row(
        Action::RangeMark,
        "v",
        "mark a range: moving marks every row passed (v or Esc ends)",
        Group::Act,
    ),
    row(
        Action::MarkPattern,
        "",
        "mark by pattern: names in this directory matching a glob",
        Group::Act,
    ),
    row(
        Action::InvertMarks,
        "",
        "invert the marks in this directory, as * does in the marks pane",
        Group::Act,
    ),
    row(
        Action::CopyPath,
        "y",
        "copy the selected path (through the terminal, OSC 52)",
        Group::Act,
    ),
    row(
        Action::OpenPath,
        "o",
        "open in the file manager (a file: its folder)",
        Group::Act,
    ),
    row(Action::Trash, "d", "move to Trash", Group::Act),
    row(
        Action::DeletePermanent,
        "D",
        "delete permanently (type delete)",
        Group::Act,
    ),
    row(
        Action::Undo,
        "U",
        "put back what was last moved to the Trash or staged",
        Group::Act,
    ),
    row(Action::Filter, "/", "filter this directory", Group::Panels),
    row(
        Action::Types,
        "t",
        "show only files of some types (video, audio, .iso...)",
        Group::Panels,
    ),
    row(
        Action::Details,
        "i",
        "details of the selected entry",
        Group::Panels,
    ),
    row(
        Action::FocusDetails,
        "Tab",
        "move focus: the list, the details, the marks pane",
        Group::Panels,
    ),
    row(
        Action::Errors,
        "!",
        "list what could not be read, and go to it",
        Group::Panels,
    ),
    row(Action::Help, "? F1", "keys", Group::Panels),
    row(
        Action::Palette,
        ": Ctrl-P",
        "command palette",
        Group::Panels,
    ),
    row(Action::Debug, "F12", "debug overlay", Group::Panels),
    row(
        Action::Close,
        "Esc",
        "close the overlay, clear the filter, or go back to Files",
        Group::Global,
    ),
    row(Action::Quit, "q Ctrl-C", "quit", Group::Global),
];

#[cfg(test)]
mod tests;
