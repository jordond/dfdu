use std::collections::HashSet;
use std::time::Instant;

use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use super::*;
use crate::tests::press;

const ALL: [Action; 61] = [
    Action::Quit,
    Action::Up,
    Action::Down,
    Action::PageUp,
    Action::PageDown,
    Action::HalfPageUp,
    Action::HalfPageDown,
    Action::First,
    Action::Last,
    Action::Open,
    Action::Enter,
    Action::Parent,
    Action::SortSize,
    Action::SortName,
    Action::SortItems,
    Action::SortMtime,
    Action::Resort,
    Action::ToggleApparent,
    Action::ToggleReferenced,
    Action::ToggleHidden,
    Action::Columns,
    Action::ToggleTree,
    Action::ExpandChain,
    Action::CycleFold,
    Action::Rescan,
    Action::EnterMount,
    Action::SkipStalled,
    Action::Trash,
    Action::DeletePermanent,
    Action::Undo,
    Action::Help,
    Action::Errors,
    Action::Close,
    Action::ToggleMark,
    Action::ClearMarks,
    Action::RangeMark,
    Action::MarkPattern,
    Action::InvertMarks,
    Action::CopyPath,
    Action::OpenPath,
    Action::Filter,
    Action::Types,
    Action::Details,
    Action::FocusDetails,
    Action::Palette,
    Action::Debug,
    Action::ViewFiles,
    Action::ViewMap,
    Action::ViewCleanup,
    Action::ViewDupes,
    Action::ViewLargest,
    Action::PrevView,
    Action::NextView,
    Action::TickSuggested,
    Action::DupesKeep,
    Action::CompareAgain,
    Action::CleanupFilter,
    Action::CleanupCommand,
    Action::TreemapDeeper,
    Action::TreemapShallower,
    Action::TreemapColor,
];

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
    let keyless: Vec<_> = keyless.iter().map(|r| r.action).collect();
    assert_eq!(keyless, [Action::MarkPattern, Action::InvertMarks]);
}

#[test]
fn first_key_is_the_first_binding() {
    assert_eq!(first_key(Action::Up), "↑");
    assert_eq!(first_key(Action::Palette), ":");
    assert_eq!(first_key(Action::ClearMarks), "X");
    assert_eq!(first_key(Action::MarkPattern), "");
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
        (KeyCode::Enter, Action::Enter),
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
        (c('c'), Action::Columns),
        (c('r'), Action::Rescan),
        (c('m'), Action::EnterMount),
        (c('x'), Action::SkipStalled),
        (c('d'), Action::Trash),
        (c('D'), Action::DeletePermanent),
        (c('U'), Action::Undo),
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
    assert_eq!(action_for(&press(c('p'))), Some(Action::TreemapColor));
    for (code, want) in [
        (c(' '), Action::ToggleMark),
        (c('/'), Action::Filter),
        (c('i'), Action::Details),
        (KeyCode::Tab, Action::FocusDetails),
        (c(':'), Action::Palette),
        (KeyCode::F(12), Action::Debug),
        (KeyCode::F(1), Action::Help),
        (c('<'), Action::Parent),
        (c('1'), Action::ViewFiles),
        (c('2'), Action::ViewMap),
        (c('3'), Action::ViewCleanup),
        (c('4'), Action::ViewDupes),
        (c('t'), Action::Types),
        (c('+'), Action::TreemapDeeper),
        (c('-'), Action::TreemapShallower),
        (c('T'), Action::ToggleTree),
        (c('*'), Action::ExpandChain),
        (c('z'), Action::CycleFold),
    ] {
        assert_eq!(action_for(&press(code)), Some(want), "{code:?}");
    }
    for code in [c('Z'), c('6'), KeyCode::F(2), KeyCode::Insert] {
        assert_eq!(action_for(&press(code)), None, "{code:?}");
    }
}

#[test]
fn navigation_and_mark_keys() {
    let c = KeyCode::Char;
    for (code, want) in [
        (c('y'), Action::CopyPath),
        (c('o'), Action::OpenPath),
        (c('v'), Action::RangeMark),
        (c('5'), Action::ViewLargest),
    ] {
        assert_eq!(action_for(&press(code)), Some(want), "{code:?}");
    }
    // Ctrl-D and Ctrl-U move half a page; without Ctrl the keys stay what they were.
    let ctrl = |ch| KeyEvent::new(c(ch), KeyModifiers::CONTROL);
    assert_eq!(action_for(&ctrl('d')), Some(Action::HalfPageDown));
    assert_eq!(action_for(&ctrl('u')), Some(Action::HalfPageUp));
    assert_eq!(action_for(&press(c('d'))), Some(Action::Trash));
    assert_eq!(action_for(&press(c('u'))), Some(Action::ToggleReferenced));
    assert_eq!(half_page(&ctrl('d'), 20), Some(10));
    assert_eq!(half_page(&ctrl('u'), 20), Some(-10));
    assert_eq!(half_page(&ctrl('d'), 1), Some(1), "at least a row");
    assert_eq!(half_page(&press(c('d')), 20), None);
}

/// What a view may do with a registry key before the registry sees it: move its own cursor
/// (the navigation keys), mark the row its cursor is on (`Space`, as in the list), and, with
/// nothing marked, say what to mark before the delete keys delete anything. Every other key
/// means the same in every view.
fn view_may_take(a: Action) -> bool {
    let nav = ACTIONS
        .iter()
        .any(|r| r.action == a && r.group == Group::Navigate);
    nav || matches!(
        a,
        Action::ToggleMark | Action::Trash | Action::DeletePermanent
    )
}

#[test]
fn no_view_rebinds_a_global_key() {
    use crate::app::views::{self, View};
    use crate::features::treemap::tests::{classified, sync};
    use crate::features::{cleanup, dupes, largest, treemap};
    let fx = classified();
    let t0 = Instant::now();
    let opened = |view: View| {
        let mut app = App::new(crate::theme::Theme::no_color(), t0);
        sync(&mut app, &fx.m, t0);
        views::switch(&mut app, view);
        sync(&mut app, &fx.m, t0);
        app
    };
    for view in [View::Map, View::Cleanup, View::Duplicates, View::Largest] {
        for info in ACTIONS {
            for key in info.keys.split_whitespace() {
                let (code, ctrl) = parse(key).expect("a key");
                let mods = if ctrl {
                    KeyModifiers::CONTROL
                } else {
                    KeyModifiers::NONE
                };
                let k = KeyEvent::new(code, mods);
                let mut app = opened(view);
                let taken = match view {
                    View::Map => treemap::key(&mut app, &k).is_some(),
                    View::Cleanup => cleanup::key(&mut app, &k).is_some(),
                    View::Duplicates => dupes::key(&mut app, &k).is_some(),
                    View::Largest => largest::key(&mut app, &k).is_some(),
                    View::Files => false,
                };
                assert!(
                    !taken || view_may_take(info.action),
                    "{view:?} takes {key}, bound to {:?}",
                    info.action
                );
            }
        }
    }
}
