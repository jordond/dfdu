//! The browser of a scan in the window: the session driver ([`Session`]) and what the window
//! keeps beside it.
//!
//! Each frame takes in a change the bridge thread saw, runs the keys pressed, ticks the session
//! (which reads the model when due), draws the toolbar, the list and the status bar from its
//! state, then carries out what was clicked or typed. It tells the session the rows the list had
//! room for, and asks egui for the next frame when the session wants to be ticked again
//! ([`Session::wake_at`]), so an idle window does not repaint.

use std::time::{Duration, Instant};

use dfdu_scan::Backend;
use dfdu_session::actions::Action;
use dfdu_session::effect::Output;
use dfdu_session::filter::{self, Edited};
use dfdu_session::line::TextOp;
use dfdu_session::palette::Token;
use dfdu_session::startup::Started;
use dfdu_session::view::View;
use dfdu_session::{FrontEnd, Session, SessionState};
use dfdu_tree::HostPath;

use crate::bridge::Bridge;
use crate::theme::Colors;
use crate::{keys, list, status_bar, toolbar};

/// How long quitting waits for the engine's threads to stop.
const SHUTDOWN: Duration = Duration::from_millis(200);

/// A browser of a scan, whatever engine backend it runs on.
pub trait Browse {
    /// Draws a frame in `ui` and carries out what the user did in it.
    fn show(&mut self, ui: &mut egui::Ui, colors: &Colors);

    fn state(&self) -> &SessionState;

    /// Stops the session and its engine ([`Session::shutdown`]).
    fn shutdown(self: Box<Self>);
}

/// What the user did in a frame, carried out once it is drawn.
#[derive(Clone, Debug, PartialEq)]
pub enum Intent {
    Act(Action),
    /// A click on a row.
    Select(usize),
    /// A double click on a row.
    Open(usize),
    /// A click on the disclosure triangle of a row in tree mode.
    ToggleRow(usize),
    /// A click on a breadcrumb: its directory opens with `child`, the one below it on the way
    /// to the current directory, selected.
    GoTo {
        child: HostPath,
    },
    /// The filter's text changed.
    Filter(String),
    /// `Enter` in the filter.
    FilterSubmit,
    /// `Esc` in the filter.
    FilterCancel,
}

/// What the window keeps beside the session's state.
#[derive(Debug, Default)]
pub struct ViewState {
    /// The filter field's text while it is edited.
    pub filter_text: String,
    /// Give the filter field the keyboard in the next frame.
    pub focus_filter: bool,
    /// Scroll the list to the selected row in the next frame.
    pub follow: bool,
}

pub struct Browser<B: Backend> {
    session: Session<'static, B>,
    bridge: Bridge,
    view: ViewState,
    /// The engine stopped with this error: its data is not shown as if it were live.
    fatal: Option<String>,
}

impl<B: Backend> Browser<B> {
    /// The browser of the engine started for a source, asking `ctx` for frames.
    pub fn new(started: Started<B>, ctx: &egui::Context) -> Self {
        let Started { engine, setup } = started;
        let mut state = SessionState::new(Instant::now());
        state.home = setup.home;
        state.read_only = setup.read_only;
        state.outside.local = setup.local;
        let session = Session::new(engine, state, setup.jobs);
        let bridge = Bridge::start(session.changes().clone(), ctx.clone());
        Browser {
            session,
            bridge,
            view: ViewState::default(),
            fatal: None,
        }
    }

    /// Carries out `intents` at `now`.
    fn apply(&mut self, intents: Vec<Intent>, now: Instant, ctx: &egui::Context) {
        for intent in intents {
            let s = self.session.state_mut();
            match intent {
                Intent::Act(a) => self.act(a, now, ctx),
                Intent::Select(row) => {
                    s.note_input(now);
                    s.status = None;
                    s.move_to(row);
                }
                Intent::Open(row) => {
                    s.move_to(row);
                    self.act(Action::Enter, now, ctx);
                }
                Intent::ToggleRow(row) => {
                    s.note_input(now);
                    s.status = None;
                    dfdu_session::tree::act::toggle_row(s, row);
                }
                Intent::GoTo { child } => {
                    s.note_input(now);
                    s.status = None;
                    let root = s.snap.root_path.clone();
                    self.view.follow |= s.go_to(&child, &root).is_some();
                }
                Intent::Filter(text) => self.set_filter(&text, now),
                Intent::FilterSubmit => {
                    s.note_input(now);
                    if filter::edit(s, TextOp::Submit) == Edited::Enter {
                        self.act(Action::Enter, now, ctx);
                    }
                }
                Intent::FilterCancel => {
                    s.note_input(now);
                    let _ = filter::edit(s, TextOp::Cancel);
                }
            }
        }
    }

    /// Carries out action `a` as its key does in the terminal.
    fn act(&mut self, a: Action, now: Instant, ctx: &egui::Context) {
        let acted = self.session.act(a, now);
        for out in acted.outputs {
            match out {
                Output::Copy(text) => ctx.copy_text(text),
                // Their actions are not offered in this version of the window.
                Output::Open { .. } | Output::Errors(_) => {}
            }
        }
        if a == Action::Filter {
            self.view.focus_filter = true;
        }
        self.view.follow |= a.moves() || acted.entered.is_some() || acted.went.is_some();
    }

    /// Sets the filter's query to `text`, opening the filter first, and brings the rows up to
    /// date with it at once.
    fn set_filter(&mut self, text: &str, now: Instant) {
        let s = self.session.state_mut();
        s.note_input(now);
        if !filter::editing(s) {
            filter::open(s);
        }
        let before = filter::selected_node(s);
        let mut changed = filter::edit(s, TextOp::Clear) == Edited::Refilter;
        for c in text.chars() {
            changed |= filter::edit(s, TextOp::Insert(c)) == Edited::Refilter;
        }
        if changed {
            s.request_refilter();
            FrontEnd::reorder(self.session.front_mut(), now);
            filter::refiltered(self.session.state_mut(), before);
        }
    }
}

impl<B: Backend> Browse for Browser<B> {
    fn show(&mut self, ui: &mut egui::Ui, colors: &Colors) {
        let ctx = ui.ctx().clone();
        let now = Instant::now();
        if let Some(e) = &self.fatal {
            egui::CentralPanel::default().show(ui, |ui| {
                ui.label(colors.text(format!("The scan stopped: {e}"), Token::StatusError));
            });
            return;
        }
        if self.bridge.take() {
            self.session.note_change();
        }
        let pressed = keys::take(&ctx).into_iter().map(Intent::Act).collect();
        self.apply(pressed, now, &ctx);
        if let Err(e) = self.session.tick(now) {
            self.fatal = Some(e.to_string());
            ctx.request_repaint();
            return;
        }
        let mut intents = Vec::new();
        let s = self.session.state();
        let view = &mut self.view;
        egui::Panel::top("toolbar").show(ui, |ui| {
            toolbar::show(ui, s, colors, view, now, &mut intents);
        });
        egui::Panel::bottom("status").show(ui, |ui| status_bar::show(ui, s, colors));
        let rows = egui::CentralPanel::default()
            .show(ui, |ui| list::show(ui, s, colors, view, &mut intents))
            .inner;
        self.session.state_mut().set_viewport(View::Files, rows);
        if !intents.is_empty() {
            self.apply(intents, now, &ctx);
            ctx.request_repaint();
        }
        self.session.shown(now);
        if let Some(t) = self.session.wake_at(now) {
            ctx.request_repaint_after(t.saturating_duration_since(now));
        }
    }

    fn state(&self) -> &SessionState {
        self.session.state()
    }

    fn shutdown(self: Box<Self>) {
        self.session.shutdown(SHUTDOWN);
    }
}
