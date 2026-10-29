//! Looking keys up in [`ACTIONS`]: the action a key runs, and the keys an action is shown with.

use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use super::{ACTIONS, Action, ActionInfo};

/// One key of [`ActionInfo::keys`]: the key code and whether it needs Ctrl.
pub(super) fn parse(key: &str) -> Option<(KeyCode, bool)> {
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

/// The rows `k` moves by when it is bound to half a page up or down, in a list that shows
/// `page` rows; `None` for any other key. The views and overlays with a cursor of their own
/// move theirs by it, as the list does.
pub fn half_page(k: &KeyEvent, page: usize) -> Option<isize> {
    let half = isize::try_from(page / 2).unwrap_or(isize::MAX).max(1);
    match action_for(k)? {
        Action::HalfPageUp => Some(-half),
        Action::HalfPageDown => Some(half),
        _ => None,
    }
}
