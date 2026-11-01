//! The browser of a scan in the window: the session driver ([`Session`]) and what the window
//! keeps beside it ([`ViewState`]).
//!
//! Each frame takes in a change the bridge thread saw, runs the keys pressed, ticks the session
//! (which reads the model when due), draws the toolbar, the status bar, the details panel, the
//! view shown (the list, the map while it has room, Cleanup, Duplicates or the largest files),
//! the delete
//! dialog, the native command pane and the windows from its state, then carries out what was
//! clicked or typed. It tells the session the rows the
//! view had room for and whether the map was drawn, and asks egui for the next frame when the
//! session wants to be ticked again ([`Session::wake_at`]), so an idle window does not repaint.

use std::time::{Duration, Instant};

use dfdu_engine::{Mode, tilde};
use dfdu_scan::Backend;
use dfdu_session::actions::Action;
use dfdu_session::confirm;
use dfdu_session::effect::Output;
use dfdu_session::filter::{self, Edited};
use dfdu_session::line::TextOp;
use dfdu_session::palette::Token;
use dfdu_session::startup::Started;
use dfdu_session::treemap;
use dfdu_session::type_filter;
use dfdu_session::view::{Then, View};
use dfdu_session::{
    FrontEnd, Session, SessionState, cleanup, dupes, error_list, file_manager, largest,
    selected_path,
};

use crate::bridge::Bridge;
use crate::help::Help;
use crate::palette::Palette;
use crate::theme::Colors;
use crate::{
    cleanup as cleanup_view, details, dialog, dupes as dupes_view, help, keys,
    largest as largest_view, list, map, palette, review, status_bar, toolbar, windows,
};

mod state;

pub use state::{Intent, ViewState};

/// How long quitting waits for the engine's threads to stop.
const SHUTDOWN: Duration = Duration::from_millis(200);

/// A browser of a scan, whatever engine backend it runs on.
pub trait Browse {
    /// Draws a frame in `ui` and carries out what the user did in it.
    fn show(&mut self, ui: &mut egui::Ui, colors: &Colors);

    fn state(&self) -> &SessionState;

    fn state_mut(&mut self) -> &mut SessionState;

    /// What the window keeps beside the session's state.
    fn view_mut(&mut self) -> &mut ViewState;

    /// Stops the session and its engine ([`Session::shutdown`]).
    fn shutdown(self: Box<Self>);
}

/// The session's state and the window's beside it.
pub struct Front {
    pub state: SessionState,
    pub view: ViewState,
}

impl FrontEnd for Front {
    fn state(&self) -> &SessionState {
        &self.state
    }

    fn state_mut(&mut self) -> &mut SessionState {
        &mut self.state
    }

    /// The type picker lists the directory's files by type.
    fn breakdown(&self) -> bool {
        self.view.types.is_some()
    }
}

pub struct Browser<B: Backend> {
    session: Session<'static, B, Front>,
    bridge: Bridge,
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
        let front = Front {
            state,
            view: ViewState::default(),
        };
        let session = Session::new(engine, front, setup.jobs);
        let bridge = Bridge::start(session.changes().clone(), ctx.clone());
        Browser {
            session,
            bridge,
            fatal: None,
        }
    }

    fn view(&mut self) -> &mut ViewState {
        &mut self.session.front_mut().view
    }

    /// Carries out `intents` at `now`.
    fn apply(&mut self, intents: Vec<Intent>, now: Instant, ctx: &egui::Context) {
        for intent in intents {
            let s = self.session.state_mut();
            s.note_input(now);
            match intent {
                Intent::Act(a) => self.act(a, now, ctx),
                Intent::Select(row) => {
                    s.status = None;
                    s.move_to(row);
                }
                Intent::Open(row) => {
                    s.move_to(row);
                    self.act(Action::Enter, now, ctx);
                }
                Intent::ToggleRow(row) => {
                    s.status = None;
                    dfdu_session::tree::act::toggle_row(s, row);
                }
                Intent::Mark(row) => {
                    s.status = None;
                    s.move_to(row);
                    dfdu_session::marks::toggle(s);
                    s.move_to(row);
                }
                Intent::GoTo { child } => {
                    s.status = None;
                    let root = s.snap.root_path.clone();
                    let went = s.go_to(&child, &root).is_some();
                    self.view().follow |= went;
                }
                Intent::Filter(text) => self.set_filter(&text, now),
                Intent::FilterSubmit => {
                    if filter::edit(s, TextOp::Submit) == Edited::Enter {
                        self.act(Action::Enter, now, ctx);
                    }
                }
                Intent::FilterCancel => {
                    let _ = filter::edit(s, TextOp::Cancel);
                }
                Intent::Confirm(i) => self.confirm(i),
                Intent::Method(mode) => {
                    if let Some(i) = s
                        .confirm
                        .as_ref()
                        .and_then(|c| dialog::switch(c.mode(), mode))
                    {
                        self.confirm(i);
                    }
                }
                Intent::Typed(text) => {
                    self.confirm(confirm::Intent::Text(TextOp::Clear));
                    for c in text.chars() {
                        self.confirm(confirm::Intent::Text(TextOp::Insert(c)));
                    }
                }
                Intent::Type(row) => type_filter::toggle(s, row),
                Intent::Toward(toward) => {
                    let Front { state, view } = self.session.front_mut();
                    match &view.map {
                        Some(tree) => treemap::step(state, tree, toward),
                        None => state.status = None,
                    }
                }
                Intent::Tile { top, open } => {
                    s.status = None;
                    let Front { state, view } = self.session.front_mut();
                    if let Some(tree) = &view.map {
                        treemap::select_top(state, tree, top);
                    }
                    if open {
                        self.act(Action::Enter, now, ctx);
                    }
                }
                Intent::MapColor(color) => treemap::set_color(s, color),
                Intent::Largest(i) => self.largest(i),
                Intent::Cleanup(i) => {
                    s.status = None;
                    let then = cleanup::act(s, i);
                    let acted = self.session.follow(then);
                    self.view().follow |= acted.went.is_some();
                }
                Intent::Dupes(i) => {
                    s.status = None;
                    let then = dupes::act(s, i);
                    let acted = self.session.follow(then);
                    self.view().follow |= acted.went.is_some();
                }
                Intent::Theme(i) => self.view().theme_pick = Some(i),
                Intent::DupesCancel => {
                    if dupes::cancel(s) {
                        s.status = Some("duplicates: the comparison was stopped".into());
                    }
                }
                Intent::Pane(i) => {
                    s.status = None;
                    let fx = cleanup::pane::act(s, i);
                    let _ = self.session.carry_out(fx);
                }
                Intent::ShowError(i) => {
                    let show = self
                        .view()
                        .errors
                        .as_ref()
                        .and_then(|l| error_list::entry(l, i));
                    if let Some(show) = show {
                        self.view().errors = None;
                        let acted = self.session.follow(Then {
                            show: Some(show),
                            ..Then::nothing()
                        });
                        self.view().follow |= acted.went.is_some();
                    }
                }
            }
        }
    }

    /// Carries out `intent` on the open delete dialog, and the delete it confirms.
    fn confirm(&mut self, intent: confirm::Intent) {
        let then = confirm::act(self.session.state_mut(), intent);
        let acted = self.session.follow(then);
        debug_assert!(
            acted.outputs.is_empty(),
            "a delete leaves the window nothing"
        );
    }

    /// Carries out `intent` in the Largest view; showing a file in Files scrolls the list to it.
    fn largest(&mut self, intent: largest::Intent) {
        let s = self.session.state_mut();
        s.status = None;
        let then = largest::act(s, intent);
        let acted = self.session.follow(then);
        self.view().follow |= acted.went.is_some();
    }

    /// Carries out action `a` as its key does in the terminal. `Esc` closes the window's own
    /// windows first; in the Largest view the keys that move, open and tick act on its files.
    fn act(&mut self, a: Action, now: Instant, ctx: &egui::Context) {
        if a == Action::Close {
            let view = self.view();
            if view.errors.take().is_some()
                || view.types.take().is_some()
                || view.help.take().is_some()
                || view.palette.take().is_some()
            {
                return;
            }
        }
        let s = self.session.state();
        if largest::active(s)
            && let Some(intent) = largest::intent(s, a)
        {
            self.largest(intent);
            self.view().follow |= a.moves();
            return;
        }
        if let Some(intent) = review::intent(s, a) {
            self.apply(vec![intent], now, ctx);
            self.view().follow |= a.moves();
            return;
        }
        let acted = self.session.act(a, now);
        if acted.front_end {
            self.front_end(a, ctx);
        }
        for out in acted.outputs {
            match out {
                Output::Copy(text) => ctx.copy_text(text),
                Output::Open { path, is_dir } => {
                    if let Err(problem) = file_manager::spawn_open(&path, is_dir) {
                        self.session.state_mut().status = Some(problem);
                    }
                }
                Output::Errors(list) => self.view().errors = Some(list),
            }
        }
        let view = self.view();
        if a == Action::Filter {
            view.focus_filter = true;
        }
        view.follow |= a.moves() || acted.entered.is_some() || acted.went.is_some();
    }

    /// The actions the session leaves to the window: copying the selected path, the type
    /// picker, the help and the command palette. The others (columns, quitting...) are not
    /// offered in this version.
    fn front_end(&mut self, a: Action, ctx: &egui::Context) {
        match a {
            Action::CopyPath => {
                let s = self.session.state_mut();
                s.status = Some(match selected_path::selected(s) {
                    Some((path, _)) => {
                        ctx.copy_text(path.to_string());
                        format!("copied {}", tilde(&path, s.home.as_deref()))
                    }
                    None => "nothing selected to copy".into(),
                });
            }
            Action::Types => {
                let view = self.view();
                view.types = match view.types {
                    Some(_) => None,
                    None => Some(String::new()),
                };
            }
            Action::Help => {
                let view = self.view();
                view.help = match view.help {
                    Some(_) => None,
                    None => Some(Help::new()),
                };
            }
            Action::Palette => self.view().palette = Some(Palette::new()),
            _ => {}
        }
    }

    /// Sets the filter's query to `text`, opening the filter first, and brings the rows up to
    /// date with it at once.
    fn set_filter(&mut self, text: &str, now: Instant) {
        let s = self.session.state_mut();
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

    /// Notes when the undo offered now was first seen, and asks for a frame each second while
    /// a staged delete counts down.
    fn note_undo(&mut self, now: Instant, ctx: &egui::Context) {
        let undo = self.session.state().snap.undo.clone();
        let view = self.view();
        match undo {
            Some(u) => {
                if view.undo_seen.is_none_or(|(plan, _)| plan != u.plan) {
                    view.undo_seen = Some((u.plan, now));
                }
                if u.mode == Mode::Stage {
                    ctx.request_repaint_after(Duration::from_secs(1));
                }
            }
            None => view.undo_seen = None,
        }
    }
}

/// Draws the view shown in the central panel, and returns which it drew with the rows it had
/// room for: the map while it has room, else the list.
fn central(
    ui: &mut egui::Ui,
    s: &SessionState,
    colors: &Colors,
    view: &mut ViewState,
    out: &mut Vec<Intent>,
) -> (View, usize) {
    match s.current_view() {
        View::Map if map::fits(ui) => {
            let rows = list::visible_rows(ui);
            map::show(ui, s, colors, view, out);
            (View::Map, rows)
        }
        View::Largest => (View::Largest, largest_view::show(ui, s, colors, view, out)),
        View::Cleanup => (View::Cleanup, cleanup_view::show(ui, s, colors, view, out)),
        View::Duplicates => (View::Duplicates, dupes_view::show(ui, s, colors, view, out)),
        _ => (View::Files, list::show(ui, s, colors, view, out)),
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
        // The delete dialog takes the keys while it is open, and the native command pane its own.
        let s = self.session.state();
        if s.confirm.is_none() {
            let pressed = if s.cleanup.pane.is_some() {
                cleanup_view::pane::keys(&ctx)
            } else {
                keys::take(&ctx, s.read_only, s.map_drawn())
            };
            self.apply(pressed, now, &ctx);
        }
        if let Err(e) = self.session.tick(now) {
            self.fatal = Some(e.to_string());
            ctx.request_repaint();
            return;
        }
        self.note_undo(now, &ctx);
        let mut intents = Vec::new();
        let Front { state: s, view } = self.session.front_mut();
        let s = &*s;
        egui::Panel::top("toolbar").show(ui, |ui| {
            toolbar::show(ui, s, colors, view, now, &mut intents);
        });
        egui::Panel::bottom("status").show(ui, |ui| {
            status_bar::show(ui, s, colors, view, now, &mut intents);
        });
        if s.details.open {
            egui::Panel::right("details")
                .resizable(true)
                .default_size(300.0)
                .show(ui, |ui| details::show(ui, s, colors));
        }
        let (shown, rows) = egui::CentralPanel::default()
            .show(ui, |ui| central(ui, s, colors, view, &mut intents))
            .inner;
        windows::errors(&ctx, colors, view, &mut intents);
        windows::types(&ctx, s, colors, view, &mut intents);
        help::show(&ctx, s.read_only, colors, view);
        palette::show(&ctx, s.read_only, colors, view, &mut intents);
        cleanup_view::pane::show(&ctx, s, colors, &mut intents);
        dialog::show(&ctx, s, colors, view, &mut intents);
        let s = self.session.state_mut();
        s.set_viewport(shown, rows);
        s.set_map_drawn(shown == View::Map);
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

    fn state_mut(&mut self) -> &mut SessionState {
        self.session.state_mut()
    }

    fn view_mut(&mut self) -> &mut ViewState {
        self.view()
    }

    fn shutdown(self: Box<Self>) {
        self.session.shutdown(SHUTDOWN);
    }
}
