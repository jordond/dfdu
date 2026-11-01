//! The status bar: the status line of the last action, or the selected entry, on the left; the
//! scan's totals and what makes them partial on the right.

use dfdu_session::SessionState;
use dfdu_session::format::{count, human, plural};
use dfdu_session::palette::Token;
use egui::{Align, Layout, Ui};

use crate::theme::Colors;

pub fn show(ui: &mut Ui, s: &SessionState, colors: &Colors) {
    ui.horizontal(|ui| {
        match (&s.status, s.selected_entry()) {
            (Some(status), _) => {
                ui.label(colors.text(status, Token::StatusWarn));
            }
            (None, Some(e)) => {
                let name = String::from_utf8_lossy(&e.name);
                ui.label(colors.text(name, Token::Fg));
                ui.label(colors.text(human(e.size), Token::SizeBig));
                if e.is_dir {
                    ui.label(colors.text(
                        format!("{} item{}", count(e.items), plural(e.items)),
                        Token::FgDim,
                    ));
                }
            }
            (None, None) => {}
        }
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            totals(ui, s, colors)
        });
    });
}

/// The scan's totals, right to left: its size and items, then errors, the filter of file types
/// and an incomplete scan.
fn totals(ui: &mut Ui, s: &SessionState, colors: &Colors) {
    let snap = &s.snap;
    let root = snap.root_total;
    ui.label(colors.text(human(s.size_mode.of(&root)), Token::Header));
    ui.label(colors.text(
        format!("{} item{}", count(root.items), plural(root.items)),
        Token::FgDim,
    ));
    if root.errors > 0 {
        ui.label(colors.text(
            format!("{} error{}", count(root.errors), plural(root.errors)),
            Token::StatusError,
        ));
    }
    if snap.incomplete {
        ui.label(colors.text("incomplete", Token::StatusWarn));
    }
    if snap.stalled > 0 {
        ui.label(colors.text(
            format!(
                "{} director{} not responding",
                snap.stalled,
                if snap.stalled == 1 { "y" } else { "ies" }
            ),
            Token::StatusWarn,
        ));
    }
}
