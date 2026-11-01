//! The native command pane (`N`), over the window while the session has one open
//! (`dfdu_session::cleanup::pane`), which holds the whole gate: an item's own cleanup command
//! starts with its dry run when it has one, and runs for real only after a dry run that worked
//! (or, without a dry run, once asked); a further-reaching form runs only when asked twice. A
//! read-only browser runs only the dry runs. It says what the terminal's pane says: the rule,
//! the form, the command verbatim, its output and where it stands.
//!
//! The pane is modal, on `raised`, and cannot be closed while its command runs. Its keys are
//! the terminal's: `y` runs it, `v` picks another form, `Esc` and `q` close it.

use dfdu_rules::schema::NativeDef;
use dfdu_session::SessionState;
use dfdu_session::cleanup::pane::{self as session, Intent as Ask, Pane};
use dfdu_session::palette::Token;
use egui::{
    CornerRadius, Event, Frame, Id, Key, Label, Layout, Margin, Modal, ScrollArea, Stroke, Ui,
};

use crate::browser::Intent;
use crate::kit::{self, Kind};
use crate::theme::{Colors, style};

/// What the pane's command did: its result once done, whether it runs, and whether it may run
/// for real now.
struct Stand<'a> {
    done: Option<&'a Result<(), String>>,
    running: bool,
    may_run: bool,
}

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
    let stand = Stand {
        done,
        running: p.id != 0 && done.is_none(),
        may_run: p.id == 0 || (p.dry_run && matches!(done, Some(Ok(())))),
    };
    let frame = Frame::new()
        .fill(colors.chrome.raised)
        .stroke(Stroke::new(1.0, colors.fg(Token::Border)))
        .corner_radius(CornerRadius::same(style::RADIUS_LG))
        .inner_margin(Margin::same(20));
    let modal = Modal::new(Id::unique("dfdu-native"))
        .frame(frame)
        .backdrop_color(colors.chrome.scrim)
        .show(ctx, |ui| {
            ui.set_width(600.0);
            ui.spacing_mut().item_spacing.y = style::SPACE_2;
            let what = match p.variant {
                None => "its cleanup command".to_string(),
                Some(v) => def
                    .variants
                    .get(v)
                    .map_or(String::new(), |x| x.name.clone()),
            };
            ui.horizontal(|ui| {
                ui.label(
                    kit::title(format!("{}:", rule.name), 15.0).color(colors.fg(Token::Title)),
                );
                ui.label(kit::sans(what, 13.0).color(colors.fg(Token::FgDim)));
            });
            let command = match p.variant {
                None => &def.command,
                Some(v) => def.variants.get(v).map_or(&def.command, |x| &x.command),
            };
            let argv = dfdu_engine::shown_argv(command);
            boxed(ui, colors, |ui| {
                let text = kit::mono(argv.clone(), 13.0).color(colors.fg(Token::Fg));
                ui.add(Label::new(text).wrap());
            });
            if let Some(output) = shown.filter(|o| !o.lines.is_empty()) {
                boxed(ui, colors, |ui| {
                    ScrollArea::vertical()
                        .max_height(280.0)
                        .stick_to_bottom(true)
                        .auto_shrink([false, true])
                        .show(ui, |ui| {
                            for l in &output.lines {
                                ui.label(kit::mono(l, 12.0).color(colors.fg(Token::Fg)));
                            }
                        });
                });
            }
            state(ui, s, p, colors, &argv, &stand);
            ui.add_space(style::SPACE_1);
            buttons(ui, s, def, colors, &stand, out);
        });
    if modal.should_close() {
        out.push(Intent::Pane(Ask::Close));
    }
}

/// A box filled in `hover` around what `add` adds: the command, its output.
fn boxed(ui: &mut Ui, colors: &Colors, add: impl FnOnce(&mut Ui)) {
    Frame::new()
        .fill(colors.chrome.hover)
        .stroke(Stroke::new(1.0, colors.chrome.divider))
        .corner_radius(CornerRadius::same(style::RADIUS_MD))
        .inner_margin(Margin::symmetric(12, 8))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            add(ui);
        });
}

/// Where the command stands, and the warning of a further-reaching form.
fn state(ui: &mut Ui, s: &SessionState, p: &Pane, colors: &Colors, argv: &str, stand: &Stand) {
    let ro = s.read_only;
    let (state, token) = match stand.done {
        None if p.id == 0 => {
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
    ui.add(Label::new(kit::sans(state, 13.0).color(colors.fg(token))).wrap());
    if p.armed {
        ui.label(
            kit::sans(
                "This form removes more than the default: press y (or Run it) again to run it.",
                12.0,
            )
            .color(colors.fg(Token::StatusWarn)),
        );
    }
}

/// The buttons, from the right: `Run it` (a delete, in `status.error`), `Other form`, `Close`.
fn buttons(
    ui: &mut Ui,
    s: &SessionState,
    def: &NativeDef,
    colors: &Colors,
    stand: &Stand,
    out: &mut Vec<Intent>,
) {
    let running = stand.running;
    ui.with_layout(Layout::right_to_left(egui::Align::Center), |ui| {
        if !s.read_only
            && kit::button(
                ui,
                colors,
                Kind::Danger,
                "Run it",
                Some("y"),
                !running && stand.may_run,
            )
            .clicked()
        {
            out.push(Intent::Pane(Ask::Run));
        }
        if !def.variants.is_empty()
            && kit::button(
                ui,
                colors,
                Kind::Secondary,
                "Other form",
                Some("v"),
                !running,
            )
            .clicked()
        {
            out.push(Intent::Pane(Ask::OtherForm));
        }
        if kit::button(ui, colors, Kind::Secondary, "Close", Some("Esc"), !running).clicked() {
            out.push(Intent::Pane(Ask::Close));
        }
    });
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
