//! The screen while a connection is made: what it does and Cancel, and the dialog of what
//! ssh or the bootstrap asks over it.

use dfdu_session::palette::Token;
use egui::{Align, Frame, Id, Key, Layout, Margin, Modal, Ui};
use zeroize::Zeroizing;

use super::{Connecting, Question};
use crate::admin::{copy, footer, header};
use crate::askpass::Kind;
use crate::kit;
use crate::secrets;
use crate::theme::Colors;
use crate::theme::style::{SPACE_2, SPACE_3};

/// The dialog's width.
const WIDTH: f32 = 460.0;

impl Connecting {
    /// The screen while it connects: what it does, and Cancel; the dialog of a question over
    /// it.
    pub fn show(&mut self, ui: &mut Ui, colors: &Colors) {
        let mut cancel = false;
        egui::CentralPanel::default()
            .frame(crate::start::frame(colors))
            .show(ui, |ui| {
                crate::start::backdrop(ui, colors);
                ui.vertical_centered(|ui| {
                    ui.add_space(ui.available_height() * 0.35);
                    ui.horizontal(|ui| {
                        let w = 18.0 + SPACE_2 + text_width(ui, &self.status);
                        ui.add_space(((ui.available_width() - w) / 2.0).max(0.0));
                        ui.spinner();
                        ui.label(kit::sans(&self.status, 13.0).color(colors.fg(Token::FgDim)));
                    });
                    ui.add_space(SPACE_3);
                    if kit::button(ui, colors, kit::Kind::Secondary, "Cancel", None, true).clicked()
                    {
                        cancel = true;
                    }
                });
            });
        if self.question.is_some() {
            self.dialog(ui.ctx(), colors);
        } else if ui.input(|i| i.key_pressed(Key::Escape)) {
            cancel = true;
        }
        if cancel {
            self.cancel();
        }
    }

    /// The dialog of the question asked.
    fn dialog(&mut self, ctx: &egui::Context, colors: &Colors) {
        let Some(q) = self.question.take() else {
            return;
        };
        let host = self.target.host.clone();
        let frame = Frame::window(&ctx.global_style()).inner_margin(Margin::ZERO);
        let mut open = true;
        // What the dialog decided: `Some(true)` goes on, `Some(false)` declines.
        let mut decided = None;
        let mut q = q;
        Modal::new(Id::unique("dfdu-remote-question"))
            .backdrop_color(colors.chrome.scrim)
            .frame(frame)
            .show(ctx, |ui| {
                ui.set_width(WIDTH);
                ui.spacing_mut().item_spacing.y = 0.0;
                let (title, line, go) = words(&q, &host);
                header(ui, colors, title, &line, &mut open);
                let body = Frame::new().inner_margin(Margin {
                    left: 24,
                    right: 24,
                    top: 0,
                    bottom: 8,
                });
                body.show(ui, |ui| {
                    ui.spacing_mut().item_spacing.y = SPACE_2;
                    let dim = colors.fg(Token::FgDim);
                    match &mut q {
                        Question::Secret {
                            ask,
                            kind,
                            input,
                            remember,
                            again,
                            id,
                        } => {
                            if *again {
                                let warn = colors.fg(Token::StatusWarn);
                                copy(ui, "That was not accepted. Try again.", warn, 12.0);
                            }
                            if *kind == Kind::Other {
                                copy(ui, &ask.prompt, dim, 12.0);
                            }
                            let field = Id::unique("dfdu-remote-secret");
                            let r = crate::windows::field_masked(
                                ui,
                                colors,
                                input,
                                field,
                                "",
                                ui.available_width(),
                                *kind != Kind::Other,
                            );
                            if !r.has_focus() && !r.lost_focus() {
                                r.request_focus();
                            }
                            if r.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter)) {
                                decided = Some(true);
                            }
                            if let (Some(store), Some(_), false) =
                                (secrets::store_name(), id, *again)
                            {
                                ui.horizontal(|ui| {
                                    let name = format!("Remember in {store}");
                                    kit::switch(ui, colors, remember, &name);
                                    ui.label(kit::sans(&name, 13.0).color(colors.fg(Token::Fg)));
                                });
                            }
                        }
                        Question::YesNo { ask, .. } => {
                            copy(ui, &ask.prompt, dim, 12.0);
                        }
                        Question::Send { .. } => {}
                    }
                });
                footer(ui, colors, |ui| {
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        ui.spacing_mut().item_spacing.x = SPACE_2;
                        if kit::button(ui, colors, kit::Kind::Primary, go, None, true).clicked() {
                            decided = Some(true);
                        }
                        if kit::button(ui, colors, kit::Kind::Secondary, "Cancel", None, true)
                            .clicked()
                        {
                            decided = Some(false);
                        }
                    });
                });
            });
        if !open || ctx.input(|i| i.key_pressed(Key::Escape)) {
            decided = decided.or(Some(false));
        }
        match decided {
            None => self.question = Some(q),
            Some(go) => self.decide(q, go),
        }
    }

    /// Answers `q`: goes on with it when `go`, otherwise declines. Declining a password stops
    /// connecting: ssh takes no answer for an empty one and would only ask again.
    pub(super) fn decide(&mut self, q: Question, go: bool) {
        match q {
            Question::Secret {
                ask,
                id,
                input,
                remember,
                ..
            } => {
                if !go {
                    ask.answer(None);
                    self.cancel();
                    return;
                }
                if let Some(id) = id {
                    self.give(id, input.clone(), remember, ask.from);
                }
                ask.answer(Some(input));
            }
            Question::YesNo { ask, .. } => {
                ask.answer(go.then(|| Zeroizing::new("yes".into())));
            }
            Question::Send { reply, .. } => {
                let _ = reply.send(go);
            }
        }
    }
}

/// The dialog's title, the line under it and the button that goes on, for `q` to `host`.
fn words(q: &Question, host: &str) -> (&'static str, String, &'static str) {
    match q {
        Question::Secret { kind, .. } => match kind {
            Kind::Password { account } => ("Password", format!("Log in to {account}."), "Log in"),
            Kind::Passphrase { key } => (
                "Key passphrase",
                format!("Unlock the key {key} to log in to {host}."),
                "Unlock",
            ),
            Kind::HostKey | Kind::Other => (
                "ssh asks",
                format!("{host} needs an answer to log in."),
                "Send",
            ),
        },
        Question::YesNo { host_key: true, .. } => (
            "Trust this computer?",
            format!("ssh does not know the key {host} showed. Trust it only if you expect it."),
            "Trust and connect",
        ),
        Question::YesNo { .. } => (
            "ssh asks",
            format!("ssh needs your yes to go on connecting to {host}."),
            "Yes",
        ),
        Question::Send { what, dir, .. } => (
            "Send the dfdu agent?",
            format!(
                "{host} has no dfdu {} to scan with. Send one ({what}) to {dir} there? It is \
                 kept for the next scan.",
                env!("CARGO_PKG_VERSION")
            ),
            "Send",
        ),
    }
}

fn text_width(ui: &Ui, text: &str) -> f32 {
    ui.fonts_mut(|f| {
        f.layout_no_wrap(
            text.to_owned(),
            egui::FontId::proportional(13.0),
            egui::Color32::WHITE,
        )
        .size()
        .x
    })
}
