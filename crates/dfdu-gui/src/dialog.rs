//! The delete dialog, drawn over the window while the session's confirmation is open
//! (`dfdu_session::confirm`), which holds the whole gate: the targets, the method, and the word
//! a permanent delete (and a large one, or one still being scanned) needs typed. It says what
//! the terminal's dialog says: what is deleted and how, its total and what the engine estimates
//! it frees.
//!
//! The dialog is modal: nothing behind it takes a click or a key. The keyboard starts on Cancel,
//! so `Enter` cancels; `Esc` and a click outside cancel too. The button that deletes stays
//! disabled until the word asked for is typed.
//!
//! It is drawn on the scrim, raised, 460 wide with 24 of room: the question as its title, the
//! method as a segmented control, the targets in mono with their sizes on the right, the
//! estimate, the field the word is typed in, and Cancel beside the button that deletes in
//! `status.error` on the right.

use dfdu_engine::{Mode, STAGE_UNDO_WINDOW};
use dfdu_session::SessionState;
use dfdu_session::confirm::{Confirm, Intent as Ask, Item, LARGE, WORD};
use dfdu_session::format::{count, human};
use dfdu_session::palette::Token;
use egui::{
    Align, CornerRadius, Frame, Id, Key, Label, Layout, Margin, Modal, RichText, Stroke, Ui, vec2,
};

use crate::browser::{Intent, ViewState};
use crate::kit::{self, Kind};
use crate::theme::Colors;
use crate::theme::style::{RADIUS_MD, SPACE_2, SPACE_3, SPACE_4, SPACE_6};

/// Targets listed before "and N more".
const SHOWN: usize = 8;
/// The field the word is typed in.
const WORD_FIELD: &str = "dfdu-delete-word";
/// The dialog's width, its room inside included.
const WIDTH: f32 = 460.0;
/// A row of the targets.
const TARGET_ROW: f32 = 20.0;

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
    let frame = Frame::window(&ctx.global_style()).inner_margin(Margin::same(24));
    let modal = Modal::new(Id::unique("dfdu-delete"))
        .backdrop_color(colors.chrome.scrim)
        .frame(frame)
        .show(ctx, |ui| {
            ui.set_width(WIDTH - 2.0 * SPACE_6);
            question(ui, c, colors);
            ui.add_space(SPACE_4);
            method(ui, c, colors, out);
            ui.add_space(SPACE_3);
            targets(ui, c, colors);
            ui.add_space(SPACE_2);
            frees(ui, s, c, colors);
            if c.typing() {
                ui.add_space(SPACE_3);
                word(ui, c, colors, view, out);
            }
            ui.add_space(SPACE_6);
            buttons(ui, c, colors, new, out);
        });
    if modal.should_close() {
        out.push(Intent::Confirm(Ask::Cancel));
    }
}

/// Copy of 12 in `t`'s colour.
fn copy(colors: &Colors, text: impl Into<String>, t: Token) -> RichText {
    kit::sans(text, 12.0).color(colors.fg(t))
}

/// What the dialog asks, for the method chosen, as its title.
fn question(ui: &mut Ui, c: &Confirm, colors: &Colors) {
    let sent: Vec<&Item> = c.sent().collect();
    let what = match sent.as_slice() {
        [one] => one.label(),
        _ => format!("{} entries", sent.len()),
    };
    let title = |ui: &mut Ui, text: String, t: Token| {
        ui.add(Label::new(kit::title(text, 15.0).color(colors.fg(t))).wrap());
    };
    match c.mode() {
        Mode::Trash => title(ui, format!("Move {what} to the Trash?"), Token::Header),
        Mode::Stage => {
            title(ui, format!("Stage {what} for deletion?"), Token::Header);
            ui.label(copy(
                colors,
                format!(
                    "Deleted after {} s; Undo puts it back until then.",
                    STAGE_UNDO_WINDOW.as_secs()
                ),
                Token::FgDim,
            ));
        }
        Mode::Permanent => {
            title(
                ui,
                format!("Permanently delete {what}?"),
                Token::StatusError,
            );
            ui.label(copy(colors, "This cannot be undone.", Token::StatusError));
        }
    }
}

/// The method, one of three; choosing another clears the word typed.
fn method(ui: &mut Ui, c: &Confirm, colors: &Colors, out: &mut Vec<Intent>) {
    let labels = METHODS.map(|(_, label)| label);
    let selected = METHODS
        .iter()
        .position(|(m, _)| *m == c.mode())
        .unwrap_or(0);
    if let Some(i) = kit::segmented(ui, colors, "Method", &labels, selected) {
        out.push(Intent::Method(METHODS[i].0));
    }
}

/// A row of the targets: `name` in mono on the left, then `note` dim, `size` on the right.
fn target_row(ui: &mut Ui, name: RichText, note: Option<RichText>, size: RichText) {
    let at = vec2(ui.available_width(), TARGET_ROW);
    ui.allocate_ui_with_layout(at, Layout::right_to_left(Align::Center), |ui| {
        ui.spacing_mut().item_spacing.x = SPACE_2;
        ui.label(size);
        if let Some(note) = note {
            ui.label(note);
        }
        ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
            ui.add(Label::new(name).truncate());
        });
    });
}

/// The targets, each with its size, then their total when there are more than one; on
/// `surface`, outlined in `divider`.
fn targets(ui: &mut Ui, c: &Confirm, colors: &Colors) {
    let dim = |t: &str| kit::mono(t, 11.0).color(colors.fg(Token::FgDim));
    let name = |t: String| kit::mono(t, 12.0).color(colors.fg(Token::Fg));
    let size = |b: u64| kit::mono(human(b), 12.0).color(colors.fg(Token::Fg));
    Frame::new()
        .fill(colors.chrome.surface)
        .stroke(Stroke::new(1.0, colors.chrome.divider))
        .corner_radius(CornerRadius::same(RADIUS_MD))
        .inner_margin(Margin::symmetric(10, 6))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.spacing_mut().item_spacing.y = 0.0;
            if let [one] = c.targets() {
                let (note, bytes) = if one.scanning {
                    (Some(dim("still scanning: at least")), one.size)
                } else {
                    (None, one.size)
                };
                target_row(ui, name(one.label()), note, size(bytes));
                return;
            }
            for it in c.targets().iter().take(SHOWN) {
                let note = if it.inside {
                    Some(dim("inside another marked item"))
                } else if it.scanning {
                    Some(dim("still scanning: at least this"))
                } else {
                    None
                };
                target_row(ui, name(it.label()), note, size(it.size));
            }
            if c.targets().len() > SHOWN {
                let more = format!("and {} more", c.targets().len() - SHOWN);
                target_row(ui, dim(&more), None, RichText::new(""));
            }
            kit::rule(ui, colors.chrome.divider, 4.0);
            let total = format!("total · {} items", count(c.items()));
            let note = c.scanning().then(|| dim("still scanning: at least this"));
            let sum = kit::mono_bold(human(c.total()), 12.0).color(colors.fg(Token::Header));
            target_row(ui, name(total), note, sum);
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
            ui.add(Label::new(copy(colors, text, token)).wrap());
        }
        None if c.shared_out() > 0 => {
            let text = format!(
                "May free less: {} of it is also used outside the scan.",
                human(c.shared_out())
            );
            ui.add(Label::new(copy(colors, text, Token::StatusWarn)).wrap());
        }
        None => {
            ui.label(copy(colors, "Estimating what it frees…", Token::FgDim));
        }
    }
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
        ui.add(Label::new(copy(colors, why, Token::FgDim)).wrap());
    }
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = SPACE_2;
        let label = ui.label(copy(colors, format!("Type {WORD} to confirm:"), Token::Fg));
        let id = Id::unique(WORD_FIELD);
        let r =
            crate::windows::field(ui, colors, &mut view.typed, id, "", 140.0).labelled_by(label.id);
        if r.changed() {
            out.push(Intent::Typed(view.typed.clone()));
        }
        if r.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter)) {
            out.push(Intent::Typed(view.typed.clone()));
            out.push(Intent::Confirm(Ask::Confirm));
        }
    });
}

/// On the right: Cancel, which has the keyboard when the dialog opens, and the button that
/// deletes, enabled once the word asked for is typed.
fn buttons(ui: &mut Ui, c: &Confirm, colors: &Colors, new: bool, out: &mut Vec<Intent>) {
    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
        ui.spacing_mut().item_spacing.x = SPACE_2;
        let ready = !c.typing() || c.typed().text() == WORD;
        let label = match c.mode() {
            Mode::Trash => "Move to Trash",
            Mode::Stage => "Stage",
            Mode::Permanent => "Delete",
        };
        if kit::button(ui, colors, Kind::Danger, label, None, ready).clicked() {
            out.push(Intent::Confirm(Ask::Confirm));
        }
        let cancel = kit::button(ui, colors, Kind::Secondary, "Cancel", Some("Esc"), true);
        if new {
            cancel.request_focus();
        }
        if cancel.clicked() {
            out.push(Intent::Confirm(Ask::Cancel));
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
