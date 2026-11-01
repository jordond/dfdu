//! The window's contents: a menu bar, then the start screen until a directory or an export is
//! chosen, then the browser of its scan, or why it could not be opened. The browser is stopped when the
//! window closes. The settings (`settings`) are applied when a browser opens and saved from it.
//! Cmd-Q and Cmd-W (Ctrl-Q, Ctrl-W) close the window. The View menu chooses the theme: the system's dark or light one, either of them, or a theme
//! file of dfdu's theme directory (`theme`), read when it is chosen.

use std::path::PathBuf;

use dfdu_session::palette::{ThemeFile, Token};
use dfdu_session::startup::{Source, StartLocal};

use crate::browser::Browse;
use crate::launch::{self, Launch, Settings};
use crate::settings::{Prefs, ThemeChoice};
use crate::start::{Chosen, Start};
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
    /// The theme file read last, by the name that chose it.
    file: Option<(String, ThemeFile)>,
    /// dfdu's theme directory, where the theme files are.
    theme_dir: Option<PathBuf>,
    /// The names of the theme files in it, as they were when the window opened.
    theme_files: Vec<String>,
}

impl Gui {
    /// The window for `launch` with the settings kept from the last run and the theme file
    /// `file` read for them, if they choose one, offering the theme files of `theme_dir`: the
    /// start screen for a local scan without a path, otherwise the browser of what it opens.
    pub fn new(
        launch: Launch,
        prefs: Prefs,
        file: Option<ThemeFile>,
        theme_dir: Option<PathBuf>,
        ctx: &egui::Context,
    ) -> Self {
        let file = match (&prefs.theme, file) {
            (ThemeChoice::File(name), Some(file)) => Some((name.clone(), file)),
            _ => None,
        };
        let Launch { source, settings } = launch;
        let theme_files = theme::listed(theme_dir.as_deref());
        let mut gui = Gui {
            screen: Screen::Closed,
            settings,
            prefs,
            file,
            theme_dir,
            theme_files,
        };
        gui.choose(gui.prefs.theme.clone(), ctx);
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
        window_keys(ui.ctx());
        egui::Panel::top("menu").show(ui, |ui| self.menu(ui));
        let colors = Colors::new(ui.ctx().theme(), self.file_in_use());
        match &mut self.screen {
            Screen::Start { start, .. } => {
                if let Some(chosen) = start.show(ui, &colors) {
                    self.chosen(chosen, ui.ctx());
                }
            }
            Screen::Browse(b) => {
                b.show(ui, &colors);
                let pick = b.view_mut().theme_pick.take();
                if let Some(choice) = pick.and_then(|i| self.themes().get(i).cloned()) {
                    self.choose(choice, ui.ctx());
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
                for choice in self.themes() {
                    if ui
                        .radio(self.prefs.theme == choice, choice.label())
                        .clicked()
                    {
                        self.choose(choice, ui.ctx());
                    }
                }
            });
        });
    }

    /// Opens what was chosen on the start screen: a folder is scanned afresh, an export is
    /// browsed read-only, as `--import` browses it.
    pub fn chosen(&mut self, chosen: Chosen, ctx: &egui::Context) {
        let Screen::Start { local, .. } = std::mem::replace(&mut self.screen, Screen::Closed)
        else {
            return;
        };
        self.screen = match chosen {
            Chosen::Folder(path) => self.open(launch::picked(path, local), ctx),
            Chosen::Export(file) => match launch::import(&file) {
                Ok(tree) => self.open(Source::Imported(tree), ctx),
                Err(e) => Screen::Failed(format!("Cannot open {}: {e}", file.display())),
            },
        };
        ctx.request_repaint();
    }

    /// The themes the menu and the palette offer: the built-in ones, then the theme files of
    /// the theme directory, with the one read at the start if it is no longer there.
    fn themes(&self) -> Vec<ThemeChoice> {
        let mut names = self.theme_files.clone();
        if let Some((name, _)) = &self.file
            && !names.contains(name)
        {
            names.push(name.clone());
        }
        let files = names.into_iter().map(ThemeChoice::File);
        ThemeChoice::BUILTIN.into_iter().chain(files).collect()
    }

    /// The theme file the theme chosen is, if it is one.
    fn file_in_use(&self) -> Option<&ThemeFile> {
        match (&self.prefs.theme, &self.file) {
            (ThemeChoice::File(chosen), Some((name, file))) if chosen == name => Some(file),
            _ => None,
        }
    }

    /// Draws with `choice` from now on, and keeps it. A theme file not read yet is read first;
    /// when it cannot be, the theme stays as it was and the status line says why.
    fn choose(&mut self, choice: ThemeChoice, ctx: &egui::Context) {
        if let ThemeChoice::File(name) = &choice
            && self.file.as_ref().is_none_or(|(read, _)| read != name)
        {
            match theme::named(name, self.theme_dir.as_deref()) {
                Ok((_, Some(file))) => self.file = Some((name.clone(), file)),
                Ok((_, None)) => {}
                Err(e) => {
                    let problem = format!("theme {name}: {e}");
                    match &mut self.screen {
                        Screen::Browse(b) => b.state_mut().status = Some(problem),
                        _ => eprintln!("dfdu-gui: {problem}"),
                    }
                    return;
                }
            }
        }
        self.prefs.theme = choice;
        let file = self.file_in_use();
        theme::apply(ctx, file);
        ctx.set_theme(theme::preference(&self.prefs.theme, file));
    }

    /// The settings to keep: the theme, and how the browser shows its list.
    pub fn prefs(&self) -> Prefs {
        match &self.screen {
            Screen::Browse(b) => Prefs {
                tree: b.state().tree.is_some(),
                details: b.state().details.open,
                ..self.prefs.clone()
            },
            _ => self.prefs.clone(),
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

    /// The browser shown, to change, if any.
    #[cfg(test)]
    pub fn browser_mut(&mut self) -> Option<&mut dyn Browse> {
        match &mut self.screen {
            Screen::Browse(b) => Some(b.as_mut()),
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
        let themes = self.themes().iter().map(|t| t.label().to_owned()).collect();
        match launch::open(source, &self.settings, ctx) {
            Ok(mut b) => {
                let s = b.state_mut();
                if self.prefs.tree {
                    dfdu_session::tree::act::toggle(s);
                }
                if self.prefs.details {
                    dfdu_session::details::toggle(s);
                }
                b.view_mut().themes = themes;
                Screen::Browse(b)
            }
            Err(e) => Screen::Failed(format!("Cannot open it: {e}")),
        }
    }
}

/// The window's own shortcuts, taken before anything else sees them: Cmd-Q (Ctrl-Q) quits and
/// Cmd-W (Ctrl-W) closes the window, which for the one window is the same. Quitting stops the
/// browser ([`Gui::close`]) as closing the window does.
fn window_keys(ctx: &egui::Context) {
    let close = ctx.input_mut(|i| {
        let quit = i.consume_key(egui::Modifiers::COMMAND, egui::Key::Q);
        quit | i.consume_key(egui::Modifiers::COMMAND, egui::Key::W)
    });
    if close {
        ctx.send_viewport_cmd(egui::ViewportCommand::Close);
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
