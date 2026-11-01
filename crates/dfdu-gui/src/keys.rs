//! The keys: the bindings of the action registry ([`ACTIONS`]) read as egui input, so a key does
//! in the window what it does in the terminal. A character (`j`, `G`, `/`) is matched against the
//! text typed, so it follows the keyboard layout; a named key (`↑`, `PgUp`, `Enter`) and `Ctrl-`
//! with a letter against the key pressed. Only the actions this version of the window offers
//! ([`OFFERED`]) are read, and in a read-only browser none that deletes or puts back.
//!
//! The platform's own shortcuts are read too: `Delete` moves to the Trash as `d` does, the
//! system's copy shortcut copies the selected path and Cmd-F (Ctrl-F) opens the filter.

use dfdu_session::actions::{ACTIONS, Action};
use egui::{Event, Key, Modifiers};

/// The actions the window offers: moving, opening, sorting, what the list shows, rescans, the
/// filters, marks, the details, the error list, the selected path, and deleting with its undo.
/// The other views, the help and the command palette are left out of this version.
pub const OFFERED: &[Action] = &[
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
    Action::ToggleHidden,
    Action::ToggleTree,
    Action::ExpandChain,
    Action::CycleFold,
    Action::Rescan,
    Action::SkipStalled,
    Action::ToggleMark,
    Action::ClearMarks,
    Action::RangeMark,
    Action::CopyPath,
    Action::OpenPath,
    Action::Trash,
    Action::DeletePermanent,
    Action::Undo,
    Action::Filter,
    Action::Types,
    Action::Details,
    Action::Errors,
    Action::Close,
];

/// What one binding of the registry matches.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
enum Binding {
    /// A character typed.
    Text(char),
    /// A key pressed, with Ctrl or without any modifier but Shift.
    Key { key: Key, ctrl: bool },
}

/// The binding a key of the registry (`k`, `↑`, `PgUp`, `Ctrl-U`) stands for; `None` for one
/// the window does not read.
fn binding(name: &str) -> Option<Binding> {
    let named = |key| Some(Binding::Key { key, ctrl: false });
    if let Some(letter) = name.strip_prefix("Ctrl-") {
        let key = Key::from_name(letter)?;
        return Some(Binding::Key { key, ctrl: true });
    }
    match name {
        "↑" => named(Key::ArrowUp),
        "↓" => named(Key::ArrowDown),
        "←" => named(Key::ArrowLeft),
        "→" => named(Key::ArrowRight),
        "PgUp" => named(Key::PageUp),
        "PgDn" => named(Key::PageDown),
        "Home" => named(Key::Home),
        "End" => named(Key::End),
        "Enter" => named(Key::Enter),
        "Backspace" => named(Key::Backspace),
        "Esc" => named(Key::Escape),
        "Space" => named(Key::Space),
        _ => {
            let mut chars = name.chars();
            match (chars.next(), chars.next()) {
                (Some(c), None) => Some(Binding::Text(c)),
                _ => None,
            }
        }
    }
}

/// The platform's shortcut `event` is, if any.
fn shortcut(event: &Event) -> Option<Action> {
    match event {
        Event::Copy => Some(Action::CopyPath),
        Event::Key {
            key: Key::Delete,
            pressed: true,
            modifiers,
            ..
        } if modifiers.is_none() => Some(Action::Trash),
        Event::Key {
            key: Key::F,
            pressed: true,
            modifiers,
            ..
        } if modifiers.command_only() => Some(Action::Filter),
        _ => None,
    }
}

/// The action offered that `event` runs, if any; in a `read_only` browser none that deletes.
pub fn action_for(event: &Event, read_only: bool) -> Option<Action> {
    let offered = |a: Action| !(read_only && a.deletes());
    if let Some(a) = shortcut(event) {
        return Some(a).filter(|&a| offered(a));
    }
    let pressed = match event {
        Event::Text(t) => {
            let mut chars = t.chars();
            match (chars.next(), chars.next()) {
                (Some(c), None) => Binding::Text(c),
                _ => return None,
            }
        }
        Event::Key {
            key,
            pressed: true,
            modifiers,
            ..
        } => Binding::Key {
            key: *key,
            ctrl: plain_or_ctrl(*modifiers)?,
        },
        _ => return None,
    };
    OFFERED.iter().copied().filter(|&a| offered(a)).find(|&a| {
        ACTIONS
            .iter()
            .filter(|info| info.action == a)
            .flat_map(|info| info.keys.split_whitespace())
            .any(|k| binding(k) == Some(pressed))
    })
}

/// Whether Ctrl is held, for a key pressed with Ctrl alone or with no modifier but Shift; `None`
/// with Alt or the Command key, whose combinations are the platform's.
fn plain_or_ctrl(m: Modifiers) -> Option<bool> {
    (!m.alt && !m.mac_cmd && (!m.command || m.ctrl)).then_some(m.ctrl)
}

/// Takes the events of this frame that run an offered action out of `ctx`'s input, so no widget
/// acts on them too, and returns their actions in order; in a `read_only` browser the keys that
/// delete are left alone. Nothing is taken while a text field has the keyboard.
pub fn take(ctx: &egui::Context, read_only: bool) -> Vec<Action> {
    if ctx.egui_wants_keyboard_input() {
        return Vec::new();
    }
    ctx.input_mut(|i| {
        let mut out = Vec::new();
        i.events.retain(|ev| match action_for(ev, read_only) {
            Some(a) => {
                out.push(a);
                false
            }
            None => true,
        });
        out
    })
}

#[cfg(test)]
mod tests;
