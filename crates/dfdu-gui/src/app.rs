//! The window's contents: a menu bar, then the start screen until a directory is chosen, then
//! the browser of its scan, or why it could not be opened. The browser is stopped when the
//! window closes. The settings (`settings`) are applied when a browser opens and saved from it.

use dfdu_session::palette::Token;
use dfdu_session::startup::{Source, StartLocal};

use crate::browser::Browse;
use crate::launch::{self, Launch, Settings};
use crate::settings::{Prefs, ThemeChoice};
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
    prefs: Prefs,
}

impl Gui {
    /// The window for `launch` with the settings kept from the last run: the start screen for
    /// a local scan without a path, otherwise the browser of what it opens.
    pub fn new(launch: Launch, prefs: Prefs, ctx: &egui::Context) -> Self {
        theme::apply(ctx);
        ctx.set_theme(prefs.theme.preference());
        let Launch { source, settings } = launch;
        let mut gui = Gui {
            screen: Screen::Closed,
            settings,
            prefs,
        };
        gui.screen = match source {
            Source::Local { path: None, start } => Screen::Start {
                start: Start::new(ctx),
                local: start,
            },
            source => gui.open(source, ctx),
        };
        gui
    }

    /// Draws a frame of the screen shown in `ui`.
    pub fn show(&mut self, ui: &mut egui::Ui) {
        egui::Panel::top("menu").show(ui, |ui| self.menu(ui));
        let colors = Colors::of(ui.ctx().theme());
        match &mut self.screen {
            Screen::Start { start, .. } => {
                if let Some(path) = start.show(ui, &colors)
                    && let Screen::Start { local, .. } =
                        std::mem::replace(&mut self.screen, Screen::Closed)
                {
                    let source = launch::picked(path, local);
                    self.screen = self.open(source, ui.ctx());
                    ui.ctx().request_repaint();
                }
            }
            Screen::Browse(b) => {
                b.show(ui, &colors);
                if let Some(i) = b.view_mut().theme_pick.take()
                    && let Some(&choice) = ThemeChoice::ALL.get(i)
                {
                    self.prefs.theme = choice;
                    ui.ctx().set_theme(choice.preference());
                }
            }
            Screen::Failed(e) => {
                egui::CentralPanel::default().show(ui, |ui| {
                    ui.label(colors.text(e.as_str(), Token::StatusError));
                });
            }
            Screen::Closed => {}
        }
    }

    /// The menu bar: the theme, and the help and the command palette of a browser.
    fn menu(&mut self, ui: &mut egui::Ui) {
        egui::MenuBar::new().ui(ui, |ui| {
            if let Screen::Browse(b) = &mut self.screen {
                ui.menu_button("Go", |ui| {
                    if ui.button("Command palette…").clicked() {
                        b.view_mut().palette = Some(crate::palette::Palette::new());
                    }
                    if ui.button("Keys").clicked() {
                        b.view_mut().help = Some(crate::help::Help::new());
                    }
                });
            }
            ui.menu_button("View", |ui| {
                ui.label("Theme");
                for choice in ThemeChoice::ALL {
                    if ui
                        .radio(self.prefs.theme == choice, choice.label())
                        .clicked()
                    {
                        self.prefs.theme = choice;
                        ui.ctx().set_theme(choice.preference());
                    }
                }
            });
        });
    }

    /// The settings to keep: the theme, and how the browser shows its list.
    pub fn prefs(&self) -> Prefs {
        match &self.screen {
            Screen::Browse(b) => Prefs {
                tree: b.state().tree.is_some(),
                details: b.state().details.open,
                ..self.prefs
            },
            _ => self.prefs,
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

    /// The browser of `source`, showing its list as the settings say, or why it could not be
    /// opened.
    fn open(&self, source: Source, ctx: &egui::Context) -> Screen {
        match launch::open(source, &self.settings, ctx) {
            Ok(mut b) => {
                let s = b.state_mut();
                if self.prefs.tree {
                    dfdu_session::tree::act::toggle(s);
                }
                if self.prefs.details {
                    dfdu_session::details::toggle(s);
                }
                b.view_mut().themes = ThemeChoice::ALL.map(|t| t.name().to_owned()).to_vec();
                Screen::Browse(b)
            }
            Err(e) => Screen::Failed(format!("Cannot open it: {e}")),
        }
    }
}

impl eframe::App for Gui {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.show(ui);
    }

    fn save(&mut self, storage: &mut dyn eframe::Storage) {
        self.prefs().save(storage);
    }

    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        self.close();
    }
}
