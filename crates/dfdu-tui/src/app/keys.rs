//! The action registry: every user action once, with its keys, label and group. Key handling,
//! the help overlay and the command palette all read [`ACTIONS`], so a binding shown anywhere is
//! the binding that works.

use std::time::Instant;

use dfdu_engine::{Command, Mode};
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use super::{App, Effect, Overlay};
use crate::order::{SortKey, SortSpec};
use crate::snapshot::SizeMode;
use crate::{confirm, debug, details, filter, marks, palette};

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
    SkipStalled,
    Trash,
    DeletePermanent,
    Help,
    Errors,
    Close,
    ToggleMark,
    ClearMarks,
    Filter,
    Details,
    FocusDetails,
    Palette,
    Debug,
}

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
    #[allow(dead_code, reason = "shown by the help overlay and the palette")]
    pub label: &'static str,
    #[allow(dead_code, reason = "shown by the help overlay and the palette")]
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
        "← h Backspace",
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
    row(Action::Filter, "/", "filter this directory", Group::Panels),
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

/// Carries out `a` on the list (no overlay open) and returns the effects for the run loop.
pub fn run(app: &mut App, a: Action, _now: Instant) -> Vec<Effect> {
    let mut fx = Vec::new();
    let page = app.list_height.max(1) as isize;
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
        Action::SkipStalled => {
            if app.snap.stalled > 0 {
                fx.push(Effect::Send(Command::AbandonAllStalled));
                app.status = Some("skipping directories that are not responding".into());
            }
        }
        Action::Trash => confirm::open(app, Mode::Trash),
        Action::DeletePermanent => confirm::open(app, Mode::Permanent),
        Action::Help => app.overlay = Overlay::Help,
        Action::Errors => fx.push(Effect::ShowErrors),
        Action::Close => {
            if !filter::clear(app) {
                app.overlay = Overlay::None;
            }
        }
        Action::ToggleMark => marks::toggle(app),
        Action::ClearMarks => marks::clear(app),
        Action::Filter => filter::open(app),
        Action::Details => details::toggle(app),
        Action::FocusDetails => details::focus(app),
        Action::Palette => palette::open(app),
        Action::Debug => debug::toggle(app),
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
fn toggle_size(app: &mut App, mode: SizeMode) {
    app.size_mode = if app.size_mode == mode {
        SizeMode::Disk
    } else {
        mode
    };
    app.request_sort();
}

#[cfg(test)]
pub(crate) mod tests {
    use std::collections::HashSet;
    use std::time::Duration;

    use dfdu_tree::Totals;

    use super::*;
    use crate::snapshot::{EntryData, Mark, Snapshot, test_entry};
    use crate::tests::render;
    use crate::theme::Theme;

    const ALL: [Action; 31] = [
        Action::Quit,
        Action::Up,
        Action::Down,
        Action::PageUp,
        Action::PageDown,
        Action::First,
        Action::Last,
        Action::Open,
        Action::Parent,
        Action::SortSize,
        Action::SortName,
        Action::SortItems,
        Action::SortMtime,
        Action::Resort,
        Action::ToggleApparent,
        Action::ToggleReferenced,
        Action::ToggleHidden,
        Action::Rescan,
        Action::SkipStalled,
        Action::Trash,
        Action::DeletePermanent,
        Action::Help,
        Action::Errors,
        Action::Close,
        Action::ToggleMark,
        Action::ClearMarks,
        Action::Filter,
        Action::Details,
        Action::FocusDetails,
        Action::Palette,
        Action::Debug,
    ];

    pub(crate) fn press(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    #[test]
    fn every_action_has_one_registry_row() {
        for a in ALL {
            let rows = ACTIONS.iter().filter(|r| r.action == a).count();
            assert_eq!(rows, 1, "{a:?}");
        }
        assert_eq!(ACTIONS.len(), ALL.len());
        let mut claimed = HashSet::new();
        for r in ACTIONS {
            assert!(!r.label.is_empty(), "{:?} has no label", r.action);
            for k in r.keys.split_whitespace() {
                let parsed = parse(k).unwrap_or_else(|| panic!("{:?}: bad key {k}", r.action));
                assert!(claimed.insert(parsed), "{k} is bound twice");
            }
        }
        let groups: HashSet<_> = ACTIONS.iter().map(|r| format!("{:?}", r.group)).collect();
        assert_eq!(groups.len(), 6, "every group has an action");
        let keyless: Vec<_> = ACTIONS.iter().filter(|r| r.keys.is_empty()).collect();
        assert!(matches!(keyless[..], [r] if r.action == Action::ClearMarks));
    }

    #[test]
    fn existing_keys_map_to_the_same_actions() {
        let c = KeyCode::Char;
        let table = [
            (c('q'), Action::Quit),
            (KeyCode::Up, Action::Up),
            (c('k'), Action::Up),
            (KeyCode::Down, Action::Down),
            (c('j'), Action::Down),
            (KeyCode::PageUp, Action::PageUp),
            (KeyCode::PageDown, Action::PageDown),
            (KeyCode::Home, Action::First),
            (c('g'), Action::First),
            (KeyCode::End, Action::Last),
            (c('G'), Action::Last),
            (KeyCode::Right, Action::Open),
            (c('l'), Action::Open),
            (KeyCode::Enter, Action::Open),
            (KeyCode::Left, Action::Parent),
            (c('h'), Action::Parent),
            (KeyCode::Backspace, Action::Parent),
            (c('s'), Action::SortSize),
            (c('n'), Action::SortName),
            (c('C'), Action::SortItems),
            (c('M'), Action::SortMtime),
            (c('S'), Action::Resort),
            (c('a'), Action::ToggleApparent),
            (c('u'), Action::ToggleReferenced),
            (c('e'), Action::ToggleHidden),
            (c('r'), Action::Rescan),
            (c('x'), Action::SkipStalled),
            (c('d'), Action::Trash),
            (c('D'), Action::DeletePermanent),
            (c('?'), Action::Help),
            (c('!'), Action::Errors),
            (KeyCode::Esc, Action::Close),
        ];
        for (code, want) in table {
            assert_eq!(action_for(&press(code)), Some(want), "{code:?}");
        }
        // Keys match whatever the modifiers: Shift for capitals, and Ctrl on a key without a
        // Ctrl binding of its own.
        let shifted = KeyEvent::new(c('G'), KeyModifiers::SHIFT);
        assert_eq!(action_for(&shifted), Some(Action::Last));
        let ctrl_j = KeyEvent::new(c('j'), KeyModifiers::CONTROL);
        assert_eq!(action_for(&ctrl_j), Some(Action::Down));
        let ctrl_p = KeyEvent::new(c('p'), KeyModifiers::CONTROL);
        assert_eq!(action_for(&ctrl_p), Some(Action::Palette));
        assert_eq!(action_for(&press(c('p'))), None);
        for (code, want) in [
            (c(' '), Action::ToggleMark),
            (c('/'), Action::Filter),
            (c('i'), Action::Details),
            (KeyCode::Tab, Action::FocusDetails),
            (c(':'), Action::Palette),
            (KeyCode::F(12), Action::Debug),
            (KeyCode::F(1), Action::Help),
        ] {
            assert_eq!(action_for(&press(code)), Some(want), "{code:?}");
        }
        for code in [c('z'), c('1'), KeyCode::F(2), KeyCode::Insert] {
            assert_eq!(action_for(&press(code)), None, "{code:?}");
        }
    }

    fn mid_scan(app: &mut App, t0: Instant) {
        let dir = |i, name: &str, size, mark| EntryData {
            is_dir: true,
            mark,
            ..test_entry(i, name, size)
        };
        let gib = 1 << 30;
        let entries = vec![
            dir(0, "Library", 112 * gib, Mark::Scanning),
            dir(1, "dev", 78 * gib, Mark::Scanning),
            dir(2, "nfs", 0, Mark::Stalled),
            test_entry(3, "notes.sqlite", 625 << 20),
        ];
        let total: u64 = entries.iter().map(|e| e.size).sum();
        let totals = Totals {
            alloc: total,
            items: 1000,
            ..Totals::default()
        };
        app.refresh(
            Snapshot {
                dir_path: "/home/jo".into(),
                dir_node: Some(test_entry(99, "/home/jo", 0).node),
                at_root: true,
                dir_total: totals,
                dir_mark: Some(Mark::Scanning),
                root_total: totals,
                entries,
                busy: true,
                stalled: 1,
                version: 1,
                ..Default::default()
            },
            t0,
        );
    }

    /// Presses each of `keys` on a mid-scan view and checks that none of them returns an
    /// effect, opens an overlay or changes the frame.
    #[track_caller]
    pub(crate) fn keys_change_nothing(keys: &[KeyEvent]) {
        let t0 = Instant::now();
        let at = t0 + Duration::from_secs(14);
        let mut app = App::new(Theme::no_color(), t0);
        mid_scan(&mut app, t0);
        let before = render(&mut app, at, 100, 12);
        for &k in keys {
            assert!(app.key(k, t0).is_empty(), "{k:?}");
            assert!(matches!(app.overlay, Overlay::None), "{k:?}");
            app.reorder(t0);
            assert_eq!(render(&mut app, at, 100, 12), before, "{k:?}");
        }
    }
}
