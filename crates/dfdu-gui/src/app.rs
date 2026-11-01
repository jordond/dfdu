//! The window's contents: the start screen until a directory is chosen, then the browser of its
//! scan, or why it could not be opened. The browser is stopped when the window closes.

use dfdu_session::palette::Token;
use dfdu_session::startup::{Source, StartLocal};

use crate::browser::Browse;
use crate::launch::{self, Launch, Settings};
use crate::start::Start;
use crate::theme::{self, Colors};

enum Screen {
    /// The start screen, with how a directory chosen on it is scanned.
    Start {
        start: Start,
        local: StartLocal,
    },
    Browse(Box<dyn Browse>),
    /// What was chosen could not be opened.
    Failed(String),
    /// Between two screens, and after the browser stopped.
    Closed,
}

pub struct Gui {
    screen: Screen,
    settings: Settings,
}

impl Gui {
    /// The window for `launch`: the start screen for a local scan without a path, otherwise the
    /// browser of what it opens.
    pub fn new(launch: Launch, ctx: &egui::Context) -> Self {
        theme::apply(ctx);
        let Launch { source, settings } = launch;
        let screen = match source {
            Source::Local { path: None, start } => Screen::Start {
                start: Start::new(ctx),
                local: start,
            },
            source => open(source, &settings, ctx),
        };
        Gui { screen, settings }
    }

    /// Draws a frame of the screen shown in `ui`.
    pub fn show(&mut self, ui: &mut egui::Ui) {
        let colors = Colors::of(ui.ctx().theme());
        match &mut self.screen {
            Screen::Start { start, .. } => {
                if let Some(path) = start.show(ui, &colors)
                    && let Screen::Start { local, .. } =
                        std::mem::replace(&mut self.screen, Screen::Closed)
                {
                    let source = launch::picked(path, local);
                    self.screen = open(source, &self.settings, ui.ctx());
                    ui.ctx().request_repaint();
                }
            }
            Screen::Browse(b) => b.show(ui, &colors),
            Screen::Failed(e) => {
                egui::CentralPanel::default().show(ui, |ui| {
                    ui.label(colors.text(e.as_str(), Token::StatusError));
                });
            }
            Screen::Closed => {}
        }
    }

    /// A browser is shown.
    pub fn browsing(&self) -> bool {
        matches!(self.screen, Screen::Browse(_))
    }

    /// The browser shown has finished its scan.
    pub fn scan_done(&self) -> bool {
        match &self.screen {
            Screen::Browse(b) => b.state().done_at.is_some() && !b.state().snap.busy,
            _ => false,
        }
    }

    /// The browser shown, if any.
    #[cfg(test)]
    pub fn browser(&self) -> Option<&dyn Browse> {
        match &self.screen {
            Screen::Browse(b) => Some(b.as_ref()),
            _ => None,
        }
    }

    /// Stops the browser, if one is shown.
    pub fn close(&mut self) {
        if let Screen::Browse(b) = std::mem::replace(&mut self.screen, Screen::Closed) {
            b.shutdown();
        }
    }
}

/// The browser of `source`, or why it could not be opened.
fn open(source: Source, settings: &Settings, ctx: &egui::Context) -> Screen {
    match launch::open(source, settings, ctx) {
        Ok(b) => Screen::Browse(b),
        Err(e) => Screen::Failed(format!("Cannot open it: {e}")),
    }
}

impl eframe::App for Gui {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.show(ui);
    }

    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        self.close();
    }
}
