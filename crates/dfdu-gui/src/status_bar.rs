//! The bars below the list. The action bar, left out of a read-only browser: what is marked and
//! its total, what the last delete to the Trash or staging can put back (a staged delete with
//! the time left to do it), and the buttons that delete. The status bar: the status line of the
//! last action, what the engine is deleting or noticed, or the selected entry, on the left; the
//! scan's totals and what makes them partial on the right, its errors a link to their list.

use std::time::Instant;

use dfdu_engine::{Mode, STAGE_UNDO_WINDOW};
use dfdu_session::SessionState;
use dfdu_session::actions::Action;
use dfdu_session::format::{count, human, plural};
use dfdu_session::palette::Token;
use egui::{Align, Button, Layout, Ui};

use crate::browser::{Intent, ViewState};
use crate::theme::Colors;

pub fn show(
    ui: &mut Ui,
    s: &SessionState,
    colors: &Colors,
    view: &ViewState,
    now: Instant,
    out: &mut Vec<Intent>,
) {
    if !s.read_only {
        ui.horizontal(|ui| actions(ui, s, colors, view, now, out));
        ui.separator();
    }
    ui.horizontal(|ui| {
        status(ui, s, colors);
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            totals(ui, s, colors, out);
        });
    });
}

/// The marks, the undo and the buttons that delete.
fn actions(
    ui: &mut Ui,
    s: &SessionState,
    colors: &Colors,
    view: &ViewState,
    now: Instant,
    out: &mut Vec<Intent>,
) {
    match marked(s) {
        Some(text) => {
            ui.label(colors.text(text, Token::Mark));
            if ui.button("Clear marks").clicked() {
                out.push(Intent::Act(Action::ClearMarks));
            }
        }
        None => {
            ui.label(colors.text("Nothing marked", Token::FgDim));
        }
    }
    if let Some(text) = undo_note(s, view, now) {
        ui.separator();
        ui.label(colors.text(text, Token::StatusWarn));
        let button = ui.add_enabled(!s.snap.undoing, Button::new("Undo"));
        if button.clicked() {
            out.push(Intent::Act(Action::Undo));
        }
    }
    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
        let target = !s.marks.is_empty() || s.selected_entry().is_some_and(|e| !e.pseudo);
        if ui
            .add_enabled(target, Button::new("Delete permanently…"))
            .clicked()
        {
            out.push(Intent::Act(Action::DeletePermanent));
        }
        if ui
            .add_enabled(target, Button::new("Move to Trash…"))
            .clicked()
        {
            out.push(Intent::Act(Action::Trash));
        }
    });
}

/// How many entries are marked and their total; `None` when nothing is.
pub fn marked(s: &SessionState) -> Option<String> {
    let marks = &s.marks;
    if marks.is_empty() {
        return None;
    }
    let (total, scanning) = marks.total();
    let at_least = if scanning { "at least " } else { "" };
    Some(format!(
        "{} marked ({at_least}{})",
        marks.items().len(),
        human(total)
    ))
}

/// What the undo puts back, and for a staged delete how long it still can: the engine deletes
/// what it staged [`STAGE_UNDO_WINDOW`] after staging it.
pub fn undo_note(s: &SessionState, view: &ViewState, now: Instant) -> Option<String> {
    let u = s.snap.undo.as_ref()?;
    let what = format!(
        "{} item{} ({})",
        u.items,
        plural(u.items as u64),
        human(u.bytes)
    );
    Some(match u.mode {
        Mode::Stage => {
            let seen = view
                .undo_seen
                .filter(|(plan, _)| *plan == u.plan)
                .map_or(now, |(_, at)| at);
            let left = STAGE_UNDO_WINDOW.saturating_sub(now.duration_since(seen));
            format!("Staged {what}: deleted in {} s", left.as_secs())
        }
        Mode::Trash => format!("Moved {what} to the Trash"),
        Mode::Permanent => format!("Deleted {what}"),
    })
}

/// The status line: a delete or an undo running, else the last action's message or the
/// engine's last notice, else the selected entry.
fn status(ui: &mut Ui, s: &SessionState, colors: &Colors) {
    let msg = match s.snap.deleting {
        Some((done, total)) => Some(format!("deleting {done}/{total}…")),
        None if s.snap.undoing => Some("putting back…".into()),
        None => s.status.clone().or_else(|| s.snap.notice.clone()),
    };
    if let Some(msg) = msg {
        ui.label(colors.text(msg, Token::StatusWarn));
        return;
    }
    if let Some(e) = s.selected_entry() {
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
}

/// The scan's totals, right to left: its size and items, then errors (a click lists them), the
/// filter of file types and an incomplete scan.
fn totals(ui: &mut Ui, s: &SessionState, colors: &Colors, out: &mut Vec<Intent>) {
    let snap = &s.snap;
    let root = snap.root_total;
    ui.label(colors.text(human(s.size_mode.of(&root)), Token::Header));
    ui.label(colors.text(
        format!("{} item{}", count(root.items), plural(root.items)),
        Token::FgDim,
    ));
    if root.errors > 0 {
        let text = format!("{} error{}", count(root.errors), plural(root.errors));
        if ui.link(colors.text(text, Token::StatusError)).clicked() {
            out.push(Intent::Act(Action::Errors));
        }
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
