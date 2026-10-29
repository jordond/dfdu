//! The action registry: every user action once, with its keys, label and group. Key handling,
//! the help overlay and the command palette all read [`ACTIONS`], so a binding shown anywhere is
//! the binding that works.

use std::time::Instant;

use dfdu_engine::{Command, Mode, SizeMode};
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use super::{App, Effect, Overlay};
use crate::features::{debug, details, treemap};
use crate::model::snapshot::Mark;
use crate::overlay::{confirm, help, palette, type_filter};
use crate::state::order::{SortKey, SortSpec};
use crate::state::{filter, marks};

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Action {
    Quit,
    Up,
    Down,
    PageUp,
    PageDown,
    First,
    Last,
    Open,
    Parent,
    SortSize,
    SortName,
    SortItems,
    SortMtime,
    Resort,
    ToggleApparent,
    ToggleReferenced,
    ToggleHidden,
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
    TreemapDeeper,
    TreemapShallower,
    TreemapColor,
}

impl Action {
    /// Deletes, or puts back what was deleted: left out of a read-only browser.
    pub fn deletes(self) -> bool {
        matches!(self, Action::Trash | Action::DeletePermanent | Action::Undo)
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
    row(Action::First, "g Home", "first entry", Group::Navigate),
    row(Action::Last, "G End", "last entry", Group::Navigate),
    row(Action::Open, "→ l Enter", "open directory", Group::Navigate),
    row(
        Action::Parent,
        "← h Backspace <",
        "parent directory",
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
        "move focus between the list and the details",
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

/// One key of [`ActionInfo::keys`]: the key code and whether it needs Ctrl.
fn parse(key: &str) -> Option<(KeyCode, bool)> {
    let mut chars = key.chars();
    if let (Some(c), None) = (chars.next(), chars.next()) {
        return Some(match c {
            '↑' => (KeyCode::Up, false),
            '↓' => (KeyCode::Down, false),
            '←' => (KeyCode::Left, false),
            '→' => (KeyCode::Right, false),
            c => (KeyCode::Char(c), false),
        });
    }
    let code = match key {
        "PgUp" => KeyCode::PageUp,
        "PgDn" => KeyCode::PageDown,
        "Home" => KeyCode::Home,
        "End" => KeyCode::End,
        "Enter" => KeyCode::Enter,
        "Backspace" => KeyCode::Backspace,
        "Esc" => KeyCode::Esc,
        "Tab" => KeyCode::Tab,
        "Space" => KeyCode::Char(' '),
        _ => {
            if let Some(n) = key.strip_prefix('F').and_then(|n| n.parse().ok()) {
                return Some((KeyCode::F(n), false));
            }
            let mut c = key.strip_prefix("Ctrl-")?.chars();
            return match (c.next(), c.next()) {
                (Some(c), None) => Some((KeyCode::Char(c.to_ascii_lowercase()), true)),
                _ => None,
            };
        }
    };
    Some((code, false))
}

/// The actions of [`ACTIONS`] the browser offers: all of them, less those that delete when it is
/// `read_only`.
pub fn offered(read_only: bool) -> impl Iterator<Item = &'static ActionInfo> {
    ACTIONS
        .iter()
        .filter(move |info| !(read_only && info.action.deletes()))
}

/// The first key of `a` in [`ACTIONS`]; empty for an action without a key.
pub fn first_key(a: Action) -> &'static str {
    ACTIONS
        .iter()
        .find(|info| info.action == a)
        .and_then(|info| info.keys.split_whitespace().next())
        .unwrap_or("")
}

/// The action bound to `k`. A `Ctrl-` binding needs Ctrl; any other binding matches its key
/// whatever the modifiers.
pub fn action_for(k: &KeyEvent) -> Option<Action> {
    let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
    let bound = |want_ctrl: bool| {
        ACTIONS.iter().find_map(|info| {
            info.keys
                .split_whitespace()
                .filter_map(parse)
                .any(|(code, needs_ctrl)| code == k.code && needs_ctrl == want_ctrl)
                .then_some(info.action)
        })
    };
    if ctrl && let Some(a) = bound(true) {
        return Some(a);
    }
    bound(false)
}

/// Carries out `a` on the list (no overlay open) and returns the effects for the run loop. In a
/// read-only browser an action that deletes only says so.
pub fn run(app: &mut App, a: Action, _now: Instant) -> Vec<Effect> {
    let mut fx = Vec::new();
    if app.read_only && a.deletes() {
        app.status = Some(READ_ONLY.into());
        return fx;
    }
    let page = isize::try_from(app.list_height.max(1)).unwrap_or(isize::MAX);
    match a {
        Action::Quit => app.quit = true,
        Action::Up => app.move_by(-1),
        Action::Down => app.move_by(1),
        Action::PageUp => app.move_by(-page),
        Action::PageDown => app.move_by(page),
        Action::First => app.move_to(0),
        Action::Last => app.move_to(app.rows.len().saturating_sub(1)),
        Action::Open => {
            if let Some(e) = app.selected_entry().filter(|e| e.is_dir).cloned() {
                app.enter(&e);
            }
        }
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
            if !filter::clear(app) && !type_filter::clear(app) {
                app.overlay = Overlay::None;
            }
        }
        Action::ToggleMark => marks::toggle(app),
        Action::ClearMarks => marks::clear(app),
        Action::Filter => filter::open(app),
        Action::Types => type_filter::open(app),
        Action::Details => details::toggle(app),
        Action::FocusDetails => details::focus(app),
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
            treemap::show(app, true);
        }
        Action::ViewCleanup => {
            crate::features::dupes::show(app, false);
            fx.extend(crate::features::cleanup::show(app, true));
        }
        Action::ViewDupes => fx.extend(crate::features::dupes::show(app, true)),
        Action::TreemapDeeper => treemap::deeper(app, 1),
        Action::TreemapShallower => treemap::deeper(app, -1),
        Action::TreemapColor => treemap::cycle_color(app),
    }
    fx
}

/// A sort key: the key's default direction, or the other direction if it is already in use.
fn sort_by(app: &mut App, key: SortKey) {
    app.sort = if app.sort.key == key {
        SortSpec {
            key,
            desc: !app.sort.desc,
        }
    } else {
        SortSpec::for_key(key)
    };
    app.request_sort();
}

/// Switches to `mode`, or back to disk usage if `mode` is shown already.
/// Asks the engine to scan into the selected mount the scan stayed out of.
fn enter_mount(app: &mut App) -> Option<Effect> {
    match app.selected_entry() {
        Some(e) if matches!(e.mark, Mark::OtherFs | Mark::Network) => {
            let name = String::from_utf8_lossy(&e.name).into_owned();
            let node = e.node;
            app.status = Some(format!("scanning {name}/ on its own filesystem…"));
            Some(Effect::Send(Command::Enter { node }))
        }
        _ => {
            app.status = Some("m scans a mount the scan stayed out of; select one first".into());
            None
        }
    }
}

fn toggle_size(app: &mut App, mode: SizeMode) {
    app.size_mode = if app.size_mode == mode {
        SizeMode::Disk
    } else {
        mode
    };
    app.request_sort();
}

#[cfg(test)]
mod tests;
