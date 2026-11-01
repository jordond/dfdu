//! The keys: the bindings of the action registry ([`ACTIONS`]) read as egui input, so a key does
//! in the window what it does in the terminal. A character (`j`, `G`, `/`) is matched against the
//! text typed, so it follows the keyboard layout; a named key (`↑`, `PgUp`, `Enter`) and `Ctrl-`
//! with a letter against the key pressed. Only the actions this version of the window offers
//! ([`OFFERED`]) are read, and none of them deletes.

use dfdu_session::actions::{ACTIONS, Action};
use egui::{Event, Key, Modifiers};

/// The actions the window offers: moving, opening, sorting, what the list shows, rescans and the
/// filter. Marks, the other views and every action that deletes are left out of this version.
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
    Action::Filter,
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

/// The action offered that `event` runs, if any.
pub fn action_for(event: &Event) -> Option<Action> {
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
    OFFERED.iter().copied().find(|&a| {
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
/// acts on them too, and returns their actions in order. Nothing is taken while a text field has
/// the keyboard.
pub fn take(ctx: &egui::Context) -> Vec<Action> {
    if ctx.egui_wants_keyboard_input() {
        return Vec::new();
    }
    ctx.input_mut(|i| {
        let mut out = Vec::new();
        i.events.retain(|ev| match action_for(ev) {
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
mod tests {
    use super::*;

    fn key(key: Key, modifiers: Modifiers) -> Event {
        Event::Key {
            key,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers,
        }
    }

    fn text(t: &str) -> Event {
        Event::Text(t.into())
    }

    #[test]
    fn the_terminal_keys_move_open_and_sort() {
        let none = Modifiers::NONE;
        assert_eq!(action_for(&text("j")), Some(Action::Down));
        assert_eq!(action_for(&key(Key::ArrowDown, none)), Some(Action::Down));
        assert_eq!(action_for(&text("k")), Some(Action::Up));
        assert_eq!(action_for(&key(Key::ArrowUp, none)), Some(Action::Up));
        assert_eq!(action_for(&key(Key::Enter, none)), Some(Action::Enter));
        assert_eq!(action_for(&text("l")), Some(Action::Open));
        assert_eq!(action_for(&key(Key::ArrowRight, none)), Some(Action::Open));
        assert_eq!(action_for(&key(Key::Backspace, none)), Some(Action::Parent));
        assert_eq!(action_for(&text("h")), Some(Action::Parent));
        assert_eq!(action_for(&text("<")), Some(Action::Parent));
        assert_eq!(action_for(&text("G")), Some(Action::Last));
        assert_eq!(action_for(&key(Key::Home, none)), Some(Action::First));
        assert_eq!(
            action_for(&key(Key::PageDown, none)),
            Some(Action::PageDown)
        );
        assert_eq!(action_for(&text("s")), Some(Action::SortSize));
        assert_eq!(action_for(&text("n")), Some(Action::SortName));
        assert_eq!(action_for(&text("M")), Some(Action::SortMtime));
        assert_eq!(action_for(&text("a")), Some(Action::ToggleApparent));
        assert_eq!(action_for(&text("e")), Some(Action::ToggleHidden));
        assert_eq!(action_for(&text("T")), Some(Action::ToggleTree));
        assert_eq!(action_for(&text("/")), Some(Action::Filter));
        assert_eq!(action_for(&key(Key::Escape, none)), Some(Action::Close));
    }

    #[test]
    fn ctrl_letters_need_ctrl_and_the_platform_shortcuts_are_left_alone() {
        assert_eq!(
            action_for(&key(Key::U, Modifiers::CTRL)),
            Some(Action::HalfPageUp)
        );
        assert_eq!(
            action_for(&key(Key::D, Modifiers::CTRL)),
            Some(Action::HalfPageDown)
        );
        assert_eq!(action_for(&key(Key::U, Modifiers::NONE)), None);
        // Cmd-↓ on a Mac, Alt-← anywhere: not the plain key.
        assert_eq!(action_for(&key(Key::ArrowDown, Modifiers::MAC_CMD)), None);
        assert_eq!(action_for(&key(Key::ArrowLeft, Modifiers::ALT)), None);
        // Shift with a named key is still the key.
        assert_eq!(
            action_for(&key(Key::ArrowDown, Modifiers::SHIFT)),
            Some(Action::Down)
        );
        let released = Event::Key {
            key: Key::Enter,
            physical_key: None,
            pressed: false,
            repeat: false,
            modifiers: Modifiers::NONE,
        };
        assert_eq!(action_for(&released), None);
    }

    #[test]
    fn nothing_offered_deletes_marks_or_quits() {
        assert!(OFFERED.iter().all(|a| !a.deletes()));
        for left_out in ["d", "D", "U", " ", "v", "X", "q", "y", "2", "t", "i", "?"] {
            assert_eq!(action_for(&text(left_out)), None, "{left_out:?}");
        }
        assert_eq!(action_for(&key(Key::Space, Modifiers::NONE)), None);
        assert_eq!(action_for(&key(Key::C, Modifiers::CTRL)), None);
    }

    #[test]
    fn every_offered_action_has_a_key_the_window_reads() {
        for &a in OFFERED {
            let info = ACTIONS.iter().find(|i| i.action == a).expect("in ACTIONS");
            assert!(
                info.keys.split_whitespace().any(|k| binding(k).is_some()),
                "{a:?}"
            );
        }
    }

    #[test]
    fn a_text_field_keeps_its_keys() {
        let ctx = egui::Context::default();
        let mut events = vec![text("j"), key(Key::ArrowDown, Modifiers::NONE)];
        let mut taken = Vec::new();
        let mut frame = |events| {
            let mut out = ctx.run_ui(
                egui::RawInput {
                    events,
                    ..Default::default()
                },
                |ui| {
                    let mut s = String::new();
                    ui.text_edit_singleline(&mut s).request_focus();
                    taken = take(ui.ctx());
                },
            );
            out.textures_delta.clear();
        };
        // The first frame gives the field the focus; from the next it has the keys.
        frame(events.clone());
        events.reverse();
        frame(events);
        assert!(taken.is_empty(), "{taken:?}");
    }
}
