//! The menu of a row (a right click): show it in the file manager, copy its path, mark it,
//! rescan it, and the two ways to delete it. A right click selects the row first, so each item
//! acts on the selected entry as its key does. A read-only browser offers no item that marks or
//! deletes, and a scan that is not of this machine (read from a file, or remote) has no file
//! manager here to show its paths in. The rows of Cleanup and Duplicates have the same menu
//! where its items apply ([`review`]): they tick rather than mark, the delete items act on what
//! is ticked in the view, and nothing there is rescanned.

use dfdu_session::SessionState;
use dfdu_session::actions::Action;
use dfdu_session::marks;
use dfdu_session::snapshot::EntryData;
use egui::Ui;

use crate::browser::Intent;
use crate::review::Tick;

/// The items of row `i`, entry `e`.
pub fn row(ui: &mut Ui, s: &SessionState, e: &EntryData, i: usize, out: &mut Vec<Intent>) {
    let mut item = |ui: &mut Ui, label: &str, intent: Intent| {
        if ui.button(label).clicked() {
            out.push(intent);
        }
    };
    if s.outside.local {
        item(ui, "Show in file manager", Intent::Act(Action::OpenPath));
    }
    item(ui, "Copy path", Intent::Act(Action::CopyPath));
    let entry = !e.pseudo && e.fold == 0;
    if !s.read_only && entry {
        let label = if marks::is_marked(s, e) {
            "Unmark"
        } else {
            "Mark"
        };
        item(ui, label, Intent::Mark(i));
    }
    item(ui, "Rescan", Intent::Act(Action::Rescan));
    if !s.read_only && entry {
        ui.separator();
        item(ui, "Move to Trash…", Intent::Act(Action::Trash));
        item(
            ui,
            "Delete permanently…",
            Intent::Act(Action::DeletePermanent),
        );
    }
}

/// The items of a row of Cleanup or Duplicates, selected by the right click: showing and
/// copying the path of an item or a copy (`path`), ticking it as its tick does (`toggle`,
/// unless it is `Locked`), and the two ways to delete what is ticked in the view.
pub fn review(
    ui: &mut Ui,
    s: &SessionState,
    path: bool,
    (tick, toggle): (Tick, &[Intent]),
    out: &mut Vec<Intent>,
) {
    let mut item = |ui: &mut Ui, label: &str, intents: &[Intent]| {
        if ui.button(label).clicked() {
            out.extend_from_slice(intents);
        }
    };
    if path && s.outside.local {
        item(ui, "Show in file manager", &[Intent::Act(Action::OpenPath)]);
    }
    if path {
        item(ui, "Copy path", &[Intent::Act(Action::CopyPath)]);
    }
    if s.read_only {
        return;
    }
    match tick {
        Tick::On => item(ui, "Untick", toggle),
        Tick::Off | Tick::Partial => item(ui, "Tick", toggle),
        Tick::Locked => {}
    }
    ui.separator();
    item(ui, "Move to Trash…", &[Intent::Act(Action::Trash)]);
    item(
        ui,
        "Delete permanently…",
        &[Intent::Act(Action::DeletePermanent)],
    );
}
