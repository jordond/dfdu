//! The action registry: every user action once, with its keys, label and group. Key handling,
//! the help overlay and the command palette all read [`ACTIONS`], so a binding shown anywhere is
//! the binding that works.

use std::time::Instant;

use dfdu_engine::{Command, Mode, SizeMode};

use super::{App, Effect, Overlay};
use crate::features::{debug, details, largest, selected_path, treemap};
use crate::overlay::{confirm, help, mark_pattern, palette, type_filter};
use crate::state::order::SortKey;
use crate::state::{filter, marks, tree};

mod act;
mod lookup;

use act::{enter_mount, sort_by, toggle_size};
#[cfg(test)]
use lookup::parse;
pub use lookup::{action_for, first_key, half_page, offered};

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
    ViewList,
    ViewTreemap,
    ViewCleanup,
    ViewDupes,
    ViewLargest,
    DupesKeep,
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
    row(Action::ViewList, "1", "list view", Group::Display),
    row(Action::ViewTreemap, "2", "treemap view", Group::Display),
    row(
        Action::ViewCleanup,
        "3",
        "cleanup view: what the rules found, by safety",
        Group::Display,
    ),
    row(
        Action::ViewDupes,
        "4",
        "duplicate files (compares the scanned files)",
        Group::Display,
    ),
    row(
        Action::ViewLargest,
        "5",
        "largest files anywhere below this directory",
        Group::Display,
    ),
    row(
        Action::DupesKeep,
        "K",
        "duplicates: change which copy is kept",
        Group::Display,
    ),
    row(
        Action::TreemapDeeper,
        "+",
        "treemap: one level deeper",
        Group::Display,
    ),
    row(
        Action::TreemapShallower,
        "-",
        "treemap: one level less",
        Group::Display,
    ),
    row(
        Action::TreemapColor,
        "p",
        "treemap: colour by top folder or age",
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
    row(Action::ClearMarks, "", "clear all marks", Group::Act),
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
        "close the overlay, or clear the filter",
        Group::Global,
    ),
    row(Action::Quit, "q Ctrl-C", "quit", Group::Global),
];

/// Carries out `a` on the list (no overlay open) and returns the effects for the run loop. In a
/// read-only browser an action that deletes only says so.
pub fn run(app: &mut App, a: Action, _now: Instant) -> Vec<Effect> {
    let mut fx = Vec::new();
    if app.read_only && a.deletes() {
        app.status = Some(READ_ONLY.into());
        return fx;
    }
    let page = isize::try_from(app.list_height.max(1)).unwrap_or(isize::MAX);
    let before = app.selected_index();
    if matches!(
        a,
        Action::ViewList | Action::ViewTreemap | Action::ViewCleanup | Action::ViewDupes
    ) {
        largest::show(app, false);
    }
    match a {
        Action::Quit => app.quit = true,
        Action::Up => app.move_by(-1),
        Action::Down => app.move_by(1),
        Action::PageUp => app.move_by(-page),
        Action::PageDown => app.move_by(page),
        Action::HalfPageUp => app.move_by(-(page / 2).max(1)),
        Action::HalfPageDown => app.move_by((page / 2).max(1)),
        Action::First => app.move_to(0),
        Action::Last => app.move_to(app.rows.len().saturating_sub(1)),
        Action::Open if tree::act::right(app) => {}
        Action::Open | Action::Enter => {
            let dir = app.selected_index().map(|row| app.rows[row]);
            if let Some(i) = dir.filter(|&i| app.snap.entries[i].is_dir) {
                app.enter(i);
            }
        }
        Action::Parent if tree::act::left(app) => {}
        Action::Parent => app.up(),
        Action::SortSize => sort_by(app, SortKey::Size),
        Action::SortName => sort_by(app, SortKey::Name),
        Action::SortItems => sort_by(app, SortKey::Items),
        Action::SortMtime => sort_by(app, SortKey::Mtime),
        Action::Resort => app.request_sort(),
        Action::ToggleApparent => toggle_size(app, SizeMode::Apparent),
        Action::ToggleReferenced => toggle_size(app, SizeMode::Referenced),
        Action::ToggleHidden => {
            app.hide_hidden = !app.hide_hidden;
            app.request_sort();
        }
        Action::Columns => {
            app.columns = app.columns.next();
            app.status = Some(app.columns.describe().into());
        }
        Action::ToggleTree => tree::act::toggle(app),
        Action::ExpandChain => tree::act::chain(app),
        Action::CycleFold => tree::act::cycle_fold(app),
        Action::Rescan => {
            let node = match app.selected_entry() {
                Some(e) if e.is_dir => Some(e.node),
                _ => app.snap.dir_node,
            };
            if let Some(node) = node {
                fx.push(Effect::Send(Command::Rescan { node }));
                app.status = Some("rescan requested".into());
            }
        }
        Action::EnterMount => fx.extend(enter_mount(app)),
        Action::SkipStalled => {
            if app.snap.stalled > 0 {
                fx.push(Effect::Send(Command::AbandonAllStalled));
                app.status = Some("skipping directories that are not responding".into());
            }
        }
        Action::Trash => fx.extend(confirm::open(app, Mode::Trash)),
        Action::DeletePermanent => fx.extend(confirm::open(app, Mode::Permanent)),
        Action::Undo => match &app.snap.undo {
            Some(u) if !app.snap.undoing => {
                fx.push(Effect::Send(Command::Undo { plan: u.plan }));
                app.status = Some(format!(
                    "putting back {} item{} ({})…",
                    u.items,
                    if u.items == 1 { "" } else { "s" },
                    crate::theme::human(u.bytes)
                ));
            }
            _ => app.status = Some("nothing to undo".into()),
        },
        Action::Help => help::open(app),
        Action::Errors => fx.push(Effect::ShowErrors),
        Action::Close => {
            if !marks::range::end(app) && !filter::clear(app) && !type_filter::clear(app) {
                app.overlay = Overlay::None;
            }
        }
        Action::ToggleMark => marks::toggle(app),
        Action::ClearMarks => marks::clear(app),
        Action::RangeMark => marks::range::toggle(app),
        Action::MarkPattern => mark_pattern::open(app),
        Action::InvertMarks => marks::invert(app),
        Action::CopyPath => fx.extend(selected_path::copy(app)),
        Action::OpenPath => fx.extend(selected_path::open(app)),
        Action::Filter => filter::open(app),
        Action::Types => type_filter::open(app),
        Action::Details => details::toggle(app),
        Action::FocusDetails => fx.extend(details::focus(app)),
        Action::Palette => palette::open(app),
        Action::Debug => debug::toggle(app),
        Action::ViewList => {
            crate::features::dupes::show(app, false);
            crate::features::cleanup::show(app, false);
            treemap::show(app, false);
        }
        Action::ViewTreemap => {
            crate::features::dupes::show(app, false);
            crate::features::cleanup::show(app, false);
            tree::act::treemap_dir(app);
            treemap::show(app, true);
        }
        Action::ViewCleanup => {
            crate::features::dupes::show(app, false);
            fx.extend(crate::features::cleanup::show(app, true));
        }
        Action::ViewDupes => fx.extend(crate::features::dupes::show(app, true)),
        Action::ViewLargest => largest::show(app, true),
        Action::DupesKeep => crate::features::dupes::cycle_keep(app),
        Action::TreemapDeeper => treemap::deeper(app, 1),
        Action::TreemapShallower => treemap::deeper(app, -1),
        Action::TreemapColor => treemap::cycle_color(app),
    }
    if a.moves() {
        marks::range::follow(app, before);
    }
    fx
}

#[cfg(test)]
mod tests;
