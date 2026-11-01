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

/// The action of `event` in a browser that deletes.
fn action(event: &Event) -> Option<Action> {
    action_for(event, false)
}

#[test]
fn the_terminal_keys_move_open_and_sort() {
    let none = Modifiers::NONE;
    assert_eq!(action(&text("j")), Some(Action::Down));
    assert_eq!(action(&key(Key::ArrowDown, none)), Some(Action::Down));
    assert_eq!(action(&text("k")), Some(Action::Up));
    assert_eq!(action(&key(Key::ArrowUp, none)), Some(Action::Up));
    assert_eq!(action(&key(Key::Enter, none)), Some(Action::Enter));
    assert_eq!(action(&text("l")), Some(Action::Open));
    assert_eq!(action(&key(Key::ArrowRight, none)), Some(Action::Open));
    assert_eq!(action(&key(Key::Backspace, none)), Some(Action::Parent));
    assert_eq!(action(&text("h")), Some(Action::Parent));
    assert_eq!(action(&text("<")), Some(Action::Parent));
    assert_eq!(action(&text("G")), Some(Action::Last));
    assert_eq!(action(&key(Key::Home, none)), Some(Action::First));
    assert_eq!(action(&key(Key::PageDown, none)), Some(Action::PageDown));
    assert_eq!(action(&text("s")), Some(Action::SortSize));
    assert_eq!(action(&text("n")), Some(Action::SortName));
    assert_eq!(action(&text("M")), Some(Action::SortMtime));
    assert_eq!(action(&text("a")), Some(Action::ToggleApparent));
    assert_eq!(action(&text("e")), Some(Action::ToggleHidden));
    assert_eq!(action(&text("T")), Some(Action::ToggleTree));
    assert_eq!(action(&text("/")), Some(Action::Filter));
    assert_eq!(action(&key(Key::Escape, none)), Some(Action::Close));
}

#[test]
fn the_terminal_keys_mark_delete_and_open_panels() {
    let none = Modifiers::NONE;
    assert_eq!(action(&key(Key::Space, none)), Some(Action::ToggleMark));
    assert_eq!(action(&text("X")), Some(Action::ClearMarks));
    assert_eq!(action(&text("v")), Some(Action::RangeMark));
    assert_eq!(action(&text("d")), Some(Action::Trash));
    assert_eq!(action(&text("D")), Some(Action::DeletePermanent));
    assert_eq!(action(&text("U")), Some(Action::Undo));
    assert_eq!(action(&text("i")), Some(Action::Details));
    assert_eq!(action(&text("t")), Some(Action::Types));
    assert_eq!(action(&text("!")), Some(Action::Errors));
    assert_eq!(action(&text("r")), Some(Action::Rescan));
    assert_eq!(action(&text("o")), Some(Action::OpenPath));
    assert_eq!(action(&text("y")), Some(Action::CopyPath));
}

#[test]
fn the_platform_shortcuts_trash_copy_and_filter() {
    assert_eq!(
        action(&key(Key::Delete, Modifiers::NONE)),
        Some(Action::Trash)
    );
    assert_eq!(action(&key(Key::Delete, Modifiers::SHIFT)), None);
    assert_eq!(action(&Event::Copy), Some(Action::CopyPath));
    assert_eq!(
        action(&key(Key::F, Modifiers::COMMAND)),
        Some(Action::Filter)
    );
}

#[test]
fn ctrl_letters_need_ctrl_and_the_platform_shortcuts_are_left_alone() {
    assert_eq!(
        action(&key(Key::U, Modifiers::CTRL)),
        Some(Action::HalfPageUp)
    );
    assert_eq!(
        action(&key(Key::D, Modifiers::CTRL)),
        Some(Action::HalfPageDown)
    );
    assert_eq!(action(&key(Key::U, Modifiers::NONE)), None);
    // Cmd-↓ on a Mac, Alt-← anywhere: not the plain key.
    assert_eq!(action(&key(Key::ArrowDown, Modifiers::MAC_CMD)), None);
    assert_eq!(action(&key(Key::ArrowLeft, Modifiers::ALT)), None);
    // Shift with a named key is still the key.
    assert_eq!(
        action(&key(Key::ArrowDown, Modifiers::SHIFT)),
        Some(Action::Down)
    );
    let released = Event::Key {
        key: Key::Enter,
        physical_key: None,
        pressed: false,
        repeat: false,
        modifiers: Modifiers::NONE,
    };
    assert_eq!(action(&released), None);
}

#[test]
fn a_read_only_browser_reads_no_key_that_deletes_or_puts_back() {
    for k in ["d", "D", "U"] {
        assert_eq!(action_for(&text(k), true), None, "{k:?}");
    }
    assert_eq!(action_for(&key(Key::Delete, Modifiers::NONE), true), None);
    // The rest still work; marking is refused by the session, which says why.
    assert_eq!(
        action_for(&key(Key::Space, Modifiers::NONE), true),
        Some(Action::ToggleMark)
    );
    assert_eq!(action_for(&text("j"), true), Some(Action::Down));
}

#[test]
fn the_views_shown_and_the_map_keys_are_read() {
    assert_eq!(action(&text("1")), Some(Action::ViewFiles));
    assert_eq!(action(&text("2")), Some(Action::ViewMap));
    assert_eq!(action(&text("5")), Some(Action::ViewLargest));
    assert_eq!(action(&text("+")), Some(Action::TreemapDeeper));
    assert_eq!(action(&text("-")), Some(Action::TreemapShallower));
    assert_eq!(action(&text("p")), Some(Action::TreemapColor));
    // On the map the arrows and `hjkl` move between tiles; Backspace still goes up.
    assert_eq!(toward(&text("h")), Some(Toward::Left));
    assert_eq!(
        toward(&key(Key::ArrowDown, Modifiers::NONE)),
        Some(Toward::Down)
    );
    assert_eq!(toward(&key(Key::ArrowUp, Modifiers::ALT)), None);
    assert_eq!(toward(&key(Key::Backspace, Modifiers::NONE)), None);
}

#[test]
fn the_other_views_the_help_and_quitting_are_left_out() {
    for left_out in ["q", "3", "4", "[", "]", "?", ":", "c", "N", "A"] {
        assert_eq!(action(&text(left_out)), None, "{left_out:?}");
    }
    assert_eq!(action(&key(Key::C, Modifiers::CTRL)), None);
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
                taken = take(ui.ctx(), false, true);
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
