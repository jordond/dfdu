//! The native command pane (`N`), over the window while the session has one open
//! (`dfdu_session::cleanup::pane`), which holds the whole gate: an item's own cleanup command
//! starts with its dry run when it has one, and runs for real only after a dry run that worked
//! (or, without a dry run, once asked); a further-reaching form runs only when asked twice. A
//! read-only browser runs only the dry runs. It says what the terminal's pane says: the rule,
//! the form, the command's output and where it stands.
//!
//! The pane is modal, and cannot be closed while its command runs. Its keys are the terminal's:
//! `y` runs it, `v` picks another form, `Esc` and `q` close it.

use dfdu_session::SessionState;
use dfdu_session::cleanup::pane::{self as session, Intent as Ask};
use dfdu_session::palette::Token;
use egui::{Button, Event, Id, Key, Modal, ScrollArea, TextStyle};

use crate::browser::Intent;
use crate::theme::Colors;

/// Draws the pane while the session has one open.
pub fn show(ctx: &egui::Context, s: &SessionState, colors: &Colors, out: &mut Vec<Intent>) {
    let Some(p) = &s.cleanup.pane else { return };
    let Some(rules) = &s.snap.rules else { return };
    let rule = rules.rule(p.rule);
    let Some(def) = rule.action.native() else {
        return;
    };
    let shown = session::output(s, p);
    let done = shown.and_then(|n| n.done.as_ref());
    let running = p.id != 0 && done.is_none();
    let may_run = p.id == 0 || (p.dry_run && matches!(done, Some(Ok(()))));
    let ro = s.read_only;
    let modal = Modal::new(Id::unique("dfdu-native")).show(ctx, |ui| {
        ui.set_max_width(640.0);
        let what = match p.variant {
            None => "its cleanup command".to_string(),
            Some(v) => def
                .variants
                .get(v)
                .map_or(String::new(), |x| x.name.clone()),
        };
        ui.horizontal(|ui| {
            ui.heading(colors.text(format!("{}:", rule.name), Token::Title));
            ui.label(what);
        });
        if let Some(output) = shown.filter(|o| !o.lines.is_empty()) {
            ScrollArea::vertical()
                .max_height(280.0)
                .stick_to_bottom(true)
                .show(ui, |ui| {
                    for l in &output.lines {
                        ui.label(colors.text(l, Token::Fg).text_style(TextStyle::Monospace));
                    }
                });
        }
        ui.separator();
        let command = match p.variant {
            None => &def.command,
            Some(v) => def.variants.get(v).map_or(&def.command, |x| &x.command),
        };
        let (state, token) = match done {
            None if p.id == 0 => {
                let argv = dfdu_engine::shown_argv(command);
                if ro {
                    (
                        format!("Would run {argv} (not in read-only mode)."),
                        Token::Fg,
                    )
                } else {
                    (format!("Runs {argv}."), Token::Fg)
                }
            }
            None => ("running…".to_string(), Token::FgDim),
            Some(Ok(())) if p.dry_run => (
                if ro {
                    "That was the dry run; read-only mode runs nothing more.".to_string()
                } else {
                    "That was the dry run.".to_string()
                },
                Token::Fg,
            ),
            Some(Ok(())) => (
                "Done; the directory is rescanned.".to_string(),
                Token::StatusOk,
            ),
            Some(Err(e)) => (format!("Failed: {e}"), Token::StatusError),
        };
        ui.label(colors.text(state, token));
        if p.armed {
            ui.label(colors.text(
                "This form removes more than the default: press y (or Run it) again to run it.",
                Token::StatusWarn,
            ));
        }
        ui.separator();
        ui.horizontal(|ui| {
            let close = ui.add_enabled(!running, Button::new("Close"));
            if close.clicked() {
                out.push(Intent::Pane(Ask::Close));
            }
            if !def.variants.is_empty()
                && ui
                    .add_enabled(!running, Button::new("Other form"))
                    .on_hover_text("key v")
                    .clicked()
            {
                out.push(Intent::Pane(Ask::OtherForm));
            }
            if !ro
                && ui
                    .add_enabled(!running && may_run, Button::new("Run it"))
                    .on_hover_text("key y")
                    .clicked()
            {
                out.push(Intent::Pane(Ask::Run));
            }
        });
    });
    if modal.should_close() {
        out.push(Intent::Pane(Ask::Close));
    }
}

/// Takes the pane's keys out of `ctx`'s input: `y` runs the command, `v` picks another form,
/// `Esc` and `q` close the pane. The other keys are left to the pane's buttons.
pub fn keys(ctx: &egui::Context) -> Vec<Intent> {
    ctx.input_mut(|i| {
        let mut out = Vec::new();
        i.events.retain(|ev| {
            let ask = match ev {
                Event::Text(t) => match t.as_str() {
                    "y" => Some(Ask::Run),
                    "v" => Some(Ask::OtherForm),
                    "q" => Some(Ask::Close),
                    _ => None,
                },
                Event::Key {
                    key: Key::Escape,
                    pressed: true,
                    ..
                } => Some(Ask::Close),
                _ => None,
            };
            out.extend(ask.map(Intent::Pane));
            ask.is_none()
        });
        out
    })
}
