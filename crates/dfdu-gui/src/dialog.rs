//! The delete dialog, drawn over the window while the session's confirmation is open
//! (`dfdu_session::confirm`), which holds the whole gate: the targets, the method, and the word
//! a permanent delete (and a large one, or one still being scanned) needs typed. It says what
//! the terminal's dialog says: what is deleted and how, its total and what the engine estimates
//! it frees.
//!
//! The dialog is modal: nothing behind it takes a click or a key. The keyboard starts on Cancel,
//! so `Enter` cancels; `Esc` and a click outside cancel too. The button that deletes stays
//! disabled until the word asked for is typed.

use dfdu_engine::{Mode, STAGE_UNDO_WINDOW};
use dfdu_session::SessionState;
use dfdu_session::confirm::{Confirm, Intent as Ask, Item, LARGE, WORD};
use dfdu_session::format::{count, human};
use dfdu_session::palette::Token;
use egui::{Button, Id, Key, Modal, TextEdit, Ui};

use crate::browser::{Intent, ViewState};
use crate::theme::Colors;

/// Targets listed before "and N more".
const SHOWN: usize = 8;
/// The field the word is typed in.
const WORD_FIELD: &str = "dfdu-delete-word";

/// The methods, as the dialog offers them.
const METHODS: [(Mode, &str); 3] = [
    (Mode::Trash, "Trash, recoverable"),
    (Mode::Stage, "Stage, then delete"),
    (Mode::Permanent, "Delete permanently"),
];

/// Draws the dialog while the session has one open.
pub fn show(
    ctx: &egui::Context,
    s: &SessionState,
    colors: &Colors,
    view: &mut ViewState,
    out: &mut Vec<Intent>,
) {
    let Some(c) = &s.confirm else {
        view.dialog = None;
        return;
    };
    let new = view.dialog != Some(c.estimate());
    view.dialog = Some(c.estimate());
    c.typed().text().clone_into(&mut view.typed);
    let modal = Modal::new(Id::unique("dfdu-delete")).show(ctx, |ui| {
        ui.set_max_width(520.0);
        question(ui, c, colors);
        targets(ui, c, colors);
        frees(ui, s, c, colors);
        ui.separator();
        method(ui, c, out);
        if c.typing() {
            word(ui, c, colors, view, out);
        }
        ui.separator();
        buttons(ui, c, new, out);
    });
    if modal.should_close() {
        out.push(Intent::Confirm(Ask::Cancel));
    }
}

/// What the dialog asks, for the method chosen.
fn question(ui: &mut Ui, c: &Confirm, colors: &Colors) {
    let sent: Vec<&Item> = c.sent().collect();
    let what = match sent.as_slice() {
        [one] => one.label(),
        _ => format!("{} entries", sent.len()),
    };
    match c.mode() {
        Mode::Trash => {
            ui.heading(format!("Move {what} to the Trash?"));
        }
        Mode::Stage => {
            ui.heading(format!("Stage {what} for deletion?"));
            ui.label(format!(
                "Deleted after {} s; Undo puts it back until then.",
                STAGE_UNDO_WINDOW.as_secs()
            ));
        }
        Mode::Permanent => {
            ui.heading(colors.text(format!("Permanently delete {what}?"), Token::StatusError));
            ui.label("This cannot be undone.");
        }
    }
}

/// The size of the one target, or the targets listed with their total.
fn targets(ui: &mut Ui, c: &Confirm, colors: &Colors) {
    let dim = |t: String| colors.text(t, Token::FgDim);
    if let [one] = c.targets() {
        ui.label(dim(if one.scanning {
            format!("at least {} (still scanning)", human(one.size))
        } else {
            human(one.size)
        }));
        return;
    }
    egui::Grid::new("dfdu-delete-targets").show(ui, |ui| {
        for it in c.targets().iter().take(SHOWN) {
            ui.label(human(it.size));
            ui.label(it.label());
            if it.inside {
                ui.label(dim("inside another marked item".into()));
            } else if it.scanning {
                ui.label(dim("still scanning: at least this".into()));
            }
            ui.end_row();
        }
        if c.targets().len() > SHOWN {
            ui.label("");
            ui.label(dim(format!("and {} more", c.targets().len() - SHOWN)));
            ui.end_row();
        }
        ui.label(colors.text(human(c.total()), Token::Header));
        ui.label(format!("total · {} items", count(c.items())));
        if c.scanning() {
            ui.label(dim("still scanning: at least this".into()));
        }
        ui.end_row();
    });
}

/// What the engine estimates the delete frees, or else what may keep it from freeing it all.
fn frees(ui: &mut Ui, s: &SessionState, c: &Confirm, colors: &Colors) {
    let estimate = s.snap.estimate.as_ref().filter(|e| e.id == c.estimate());
    match estimate {
        Some(e) => {
            let mut text = format!(
                "Frees {}{}",
                if e.scanning { "at least " } else { "about " },
                human(e.bytes)
            );
            let stays: Vec<String> = [
                (e.linked_out, "hard-linked from outside"),
                (e.shared_out, "shared with data outside the scan"),
            ]
            .iter()
            .filter(|(b, _)| *b > 0)
            .map(|(b, why)| format!("{} {why}", human(*b)))
            .collect();
            if !stays.is_empty() {
                text.push_str("; stays: ");
                text.push_str(&stays.join(", "));
            }
            if e.may_free_less {
                text.push_str("; may free less (clones)");
            }
            let token = if e.linked_out + e.shared_out > 0 {
                Token::StatusWarn
            } else {
                Token::FgDim
            };
            ui.label(colors.text(text, token));
        }
        None if c.shared_out() > 0 => {
            ui.label(colors.text(
                format!(
                    "May free less: {} of it is also used outside the scan.",
                    human(c.shared_out())
                ),
                Token::StatusWarn,
            ));
        }
        None => {
            ui.label(colors.text("Estimating what it frees…", Token::FgDim));
        }
    }
}

/// The method, one of three; choosing another clears the word typed.
fn method(ui: &mut Ui, c: &Confirm, out: &mut Vec<Intent>) {
    ui.horizontal(|ui| {
        ui.label("Method:");
        for (mode, label) in METHODS {
            if ui.radio(c.mode() == mode, label).clicked() && c.mode() != mode {
                out.push(Intent::Method(mode));
            }
        }
    });
}

/// The field the word is typed in, and why it is asked for.
fn word(ui: &mut Ui, c: &Confirm, colors: &Colors, view: &mut ViewState, out: &mut Vec<Intent>) {
    if c.mode() != Mode::Permanent {
        let why = if c.total() >= LARGE {
            format!("{} or more: confirm by typing the word.", human(LARGE))
        } else {
            format!(
                "Still scanning, may be {} or more: confirm by typing the word.",
                human(LARGE)
            )
        };
        ui.label(colors.text(why, Token::FgDim));
    }
    ui.horizontal(|ui| {
        let label = ui.label(format!("Type {WORD} to confirm:"));
        let r = ui
            .add(
                TextEdit::singleline(&mut view.typed)
                    .id(Id::unique(WORD_FIELD))
                    .desired_width(120.0),
            )
            .labelled_by(label.id);
        if r.changed() {
            out.push(Intent::Typed(view.typed.clone()));
        }
        if r.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter)) {
            out.push(Intent::Typed(view.typed.clone()));
            out.push(Intent::Confirm(Ask::Confirm));
        }
    });
}

/// Cancel, which has the keyboard when the dialog opens, and the button that deletes, enabled
/// once the word asked for is typed.
fn buttons(ui: &mut Ui, c: &Confirm, new: bool, out: &mut Vec<Intent>) {
    ui.horizontal(|ui| {
        let cancel = ui.button("Cancel");
        if new {
            cancel.request_focus();
        }
        if cancel.clicked() {
            out.push(Intent::Confirm(Ask::Cancel));
        }
        let ready = !c.typing() || c.typed().text() == WORD;
        let label = match c.mode() {
            Mode::Trash => "Move to Trash",
            Mode::Stage => "Stage",
            Mode::Permanent => "Delete",
        };
        if ui.add_enabled(ready, Button::new(label)).clicked() {
            out.push(Intent::Confirm(Ask::Confirm));
        }
    });
}

/// The intent that switches the dialog's method from `from` to `to`, if they differ.
pub fn switch(from: Mode, to: Mode) -> Option<Ask> {
    if from == to {
        None
    } else if to == Mode::Permanent || from == Mode::Permanent && to == Mode::Trash {
        Some(Ask::SwitchPermanent)
    } else {
        Some(Ask::SwitchStage)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_method_is_reached_from_each_other_in_one_switch() {
        let all = [Mode::Trash, Mode::Stage, Mode::Permanent];
        for from in all {
            for to in all {
                let mode = match switch(from, to) {
                    None => from,
                    Some(Ask::SwitchPermanent) => match from {
                        Mode::Trash | Mode::Stage => Mode::Permanent,
                        Mode::Permanent => Mode::Trash,
                    },
                    Some(Ask::SwitchStage) => match from {
                        Mode::Stage => Mode::Trash,
                        _ => Mode::Stage,
                    },
                    Some(other) => panic!("{other:?}"),
                };
                assert_eq!(mode, to, "{from:?} to {to:?}");
            }
        }
    }
}
