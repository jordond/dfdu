//! The window's contents: the start screen until a directory or an export is chosen, with a
//! View menu in its top right corner, then the browser of its scan, or why it could not be
//! opened. The browser is stopped when the
//! window closes. The settings (`settings`) are applied when a browser opens and saved from it.
//! Cmd-Q and Cmd-W (Ctrl-Q, Ctrl-W) close the window. The View menu chooses the theme: the system's dark or light one, either of them, or a theme
//! file of dfdu's theme directory (`theme`), read when it is chosen.
//!
//! A browser that asks for a scan of a folder outside its own (up past its root, a folder gone
//! to, a breadcrumb above the root) gets one opened the way its own was ([`Origin`]): on this
//! machine as `dfdu PATH` opens it, as an administrator, or over ssh to the same computer. The
//! new scan is opened first and the old browser stopped only once it is; when it cannot be
//! opened, the old browser shows again with why.

use std::io;
use std::path::PathBuf;

use dfdu_session::palette::{ThemeFile, Token};
use dfdu_session::startup::{Source, StartLocal};
use dfdu_tree::HostPath;

use crate::browser::Browse;
use crate::kit;
use crate::launch::{self, Launch, Settings};
use crate::settings::{Prefs, ThemeChoice};
use crate::start::{self, Chosen, Start};
use crate::theme::{self, Colors};

/// How a browser's scan was opened, so a scan of another folder is opened the same way.
#[derive(Clone, Debug)]
enum Origin {
    /// On this machine.
    Local,
    /// On this machine as an administrator.
    Admin,
    /// On another computer, connected to as `Target` says.
    Remote(crate::remote::Target),
    /// Read from a file: nothing outside it can be opened.
    Imported,
}

/// The screen to show again when what is being opened fails or is cancelled.
enum Back {
    /// The start screen, with how a directory chosen on it is scanned.
    Start(Start, StartLocal),
    /// The browser that asked for a scan of another folder, stopped once the new one opens.
    Browse(Box<dyn Browse>, Origin),
}

impl Back {
    /// The browser to stop once the new one opens.
    fn into_browser(self) -> Option<(Box<dyn Browse>, Origin)> {
        match self {
            Back::Browse(b, origin) => Some((b, origin)),
            Back::Start(..) => None,
        }
    }
}

enum Screen {
    /// The start screen, with how a directory chosen on it is scanned.
    Start {
        start: Start,
        local: StartLocal,
    },
    Browse(Box<dyn Browse>, Origin),
    /// An administrator's scan of `path`, waiting for the system's password dialog and the
    /// agent ([`launch::elevate`]).
    Elevating {
        path: PathBuf,
        opened: crossbeam_channel::Receiver<launch::Elevating>,
        /// The screen to show again if the password dialog is cancelled.
        back: Option<Back>,
    },
    /// A scan of another computer, while ssh connects and the agent starts there
    /// ([`crate::remote`]).
    Connecting {
        conn: Box<crate::remote::Connecting>,
        /// The screen to show again if it fails or is cancelled.
        back: Back,
    },
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
    /// The Full disk access window, while open ([`crate::admin`]).
    admin: Option<crate::admin::Panel>,
    /// The update check, its notice and the Updates window ([`crate::update`]).
    updates: crate::update::Updates,
    /// The ssh passwords given in this run ([`crate::secrets`]).
    secrets: crate::secrets::Secrets,
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
        let Launch {
            source,
            settings,
            admin,
        } = launch;
        let theme_files = theme::listed(theme_dir.as_deref());
        theme::install(ctx);
        let mut gui = Gui {
            screen: Screen::Closed,
            settings,
            prefs,
            file,
            theme_dir,
            theme_files,
            admin: None,
            updates: crate::update::Updates::new(),
            secrets: crate::secrets::Secrets::default(),
        };
        gui.choose(gui.prefs.theme.clone(), ctx);
        gui.screen = match source {
            Source::Local { path: None, start } => {
                let mut s = Start::new(ctx);
                s.admin = admin;
                Screen::Start {
                    start: s,
                    local: start,
                }
            }
            Source::Local {
                path: Some(path),
                start,
            } if admin => {
                let mut back = Start::new(ctx);
                back.admin = true;
                gui.elevate(path, Some(Back::Start(back, start)), ctx)
            }
            source => {
                let origin = match source {
                    Source::Imported(_) => Origin::Imported,
                    _ => Origin::Local,
                };
                gui.open(source, origin, None, ctx)
            }
        };
        gui
    }

    /// Draws a frame of the screen shown in `ui`.
    pub fn show(&mut self, ui: &mut egui::Ui) {
        window_keys(ui.ctx());
        let colors = Colors::new(ui.ctx().theme(), self.file_in_use());
        let current = self.themes().iter().position(|t| *t == self.prefs.theme);
        let mut reopen = None;
        match &mut self.screen {
            Screen::Start { start, .. } => {
                if let Some(w) = self.secrets.warning() {
                    start.remote_error = Some(w);
                }
                let chosen = start.show(ui, &colors, &self.prefs.remotes);
                if std::mem::take(&mut start.setup) {
                    self.admin = Some(crate::admin::Panel::new());
                }
                if let Some(chosen) = chosen {
                    self.chosen(chosen, ui.ctx());
                }
            }
            Screen::Browse(b, _) => {
                b.view_mut().theme_current = current;
                if let Some(w) = self.secrets.warning() {
                    b.state_mut().status = Some(w);
                }
                b.show(ui, &colors);
                if std::mem::take(&mut b.view_mut().admin_open) {
                    self.admin = Some(crate::admin::Panel::new());
                }
                if std::mem::take(&mut b.view_mut().updates_open) {
                    self.updates.open(ui.ctx());
                }
                let pick = b.view_mut().theme_pick.take();
                reopen = b.view_mut().reopen.take();
                if let Some(choice) = pick.and_then(|i| self.themes().get(i).cloned()) {
                    self.choose(choice, ui.ctx());
                }
            }
            Screen::Elevating { path, opened, back } => match opened.try_recv() {
                Ok(Ok(elevated)) => {
                    let old = back.take().and_then(Back::into_browser);
                    self.screen = self.open(elevated.source(), Origin::Admin, old, ui.ctx());
                }
                Ok(Err(why)) => {
                    self.screen = match (back.take(), why) {
                        (Some(Back::Browse(mut b, origin)), why) => {
                            b.state_mut().status = why;
                            Screen::Browse(b, origin)
                        }
                        (_, Some(e)) => Screen::Failed(e),
                        (Some(Back::Start(start, local)), None) => Screen::Start { start, local },
                        (None, None) => Screen::Closed,
                    };
                    ui.ctx().request_repaint();
                }
                Err(_) => {
                    let text = format!(
                        "Waiting for an administrator's password to scan {}…",
                        path.display()
                    );
                    message(ui, &colors, &text, colors.fg(Token::FgDim));
                }
            },
            Screen::Connecting { conn, .. } => {
                conn.show(ui, &colors);
                if let Some(outcome) = conn.poll(&mut self.secrets, ui.ctx()) {
                    self.connected(outcome, ui.ctx());
                }
            }
            Screen::Failed(e) => message(ui, &colors, e, colors.fg(Token::StatusError)),
            Screen::Closed => {}
        }
        if let Some(path) = reopen {
            self.reopen(path, ui.ctx());
        }
        // The browser's toolbar has its own View menu.
        if !self.browsing() {
            self.menu(ui.ctx(), &colors);
        }
        if let Some(panel) = &mut self.admin
            && !panel.show(ui.ctx(), &colors)
        {
            self.admin = None;
        }
        self.updates.tick(ui.ctx(), &mut self.prefs);
        self.updates.notice(ui.ctx(), &colors, &mut self.prefs);
        self.updates.window(ui.ctx(), &colors, &mut self.prefs);
    }

    /// The View menu of the screens without a browser, quiet in the window's top right corner:
    /// Full disk access, Updates and the theme.
    fn menu(&mut self, ctx: &egui::Context, colors: &Colors) {
        let quiet = egui::Button::new(kit::label("View", 12.0).color(colors.fg(Token::FgDim)))
            .frame_when_inactive(false);
        egui::Area::new(egui::Id::unique("dfdu-view-menu"))
            .anchor(egui::Align2::RIGHT_TOP, egui::vec2(-16.0, 12.0))
            .order(egui::Order::Foreground)
            .show(ctx, |ui| {
                egui::containers::menu::MenuButton::from_button(quiet).ui(ui, |ui| {
                    if ui.button("Full disk access…").clicked() {
                        self.admin = Some(crate::admin::Panel::new());
                        ui.close();
                    }
                    if ui.button("Updates and command-line tool…").clicked() {
                        self.updates.open(ui.ctx());
                        ui.close();
                    }
                    ui.separator();
                    ui.label(kit::label("Theme", 11.0).weak());
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
        let Screen::Start { local, mut start } =
            std::mem::replace(&mut self.screen, Screen::Closed)
        else {
            return;
        };
        self.screen = match chosen {
            Chosen::Folder(_) if start.admin && !crate::admin::step().usable() => {
                self.admin = Some(crate::admin::Panel::new());
                Screen::Start { start, local }
            }
            Chosen::Folder(path) if start.admin => {
                self.elevate(path, Some(Back::Start(start, local)), ctx)
            }
            Chosen::Folder(path) => {
                self.open(launch::picked(path, local), Origin::Local, None, ctx)
            }
            Chosen::Remote(target) => {
                start.remote_error = None;
                let conn = crate::remote::Connecting::start(target, &self.settings, ctx);
                Screen::Connecting {
                    conn: Box::new(conn),
                    back: Back::Start(start, local),
                }
            }
            Chosen::Export(file) => match launch::import(&file) {
                Ok(tree) => self.open(Source::Imported(tree), Origin::Imported, None, ctx),
                Err(e) => Screen::Failed(format!("Cannot open {}: {e}", file.display())),
            },
        };
        ctx.request_repaint();
    }

    /// Opens the scan of the other computer connected to, or shows the screen it was asked
    /// from again with why it could not.
    fn connected(&mut self, outcome: crate::remote::Outcome, ctx: &egui::Context) {
        let Screen::Connecting { conn, back } = std::mem::replace(&mut self.screen, Screen::Closed)
        else {
            return;
        };
        let target = conn.target.clone();
        self.screen = match (outcome, back) {
            (crate::remote::Outcome::Opened(opened), back) => {
                crate::remote::remember(&mut self.prefs.remotes, &target);
                let old = back.into_browser();
                self.open(opened.source(), Origin::Remote(target), old, ctx)
            }
            (crate::remote::Outcome::Failed(why), Back::Start(mut start, local)) => {
                start.failed_remote(&target, why);
                Screen::Start { start, local }
            }
            (crate::remote::Outcome::Failed(why), Back::Browse(mut b, origin)) => {
                if !why.is_empty() {
                    b.state_mut().status = Some(format!("cannot scan {}: {why}", target.path));
                }
                Screen::Browse(b, origin)
            }
        };
        ctx.request_repaint();
    }

    /// Opens a scan of `path`, which the browser shown asked for, the way the browser's own
    /// was opened and with the settings it shows its list with. The browser is stopped once
    /// the new scan opens, and shows why when it cannot.
    fn reopen(&mut self, path: HostPath, ctx: &egui::Context) {
        // The list as the browser shows it now, for the new one.
        self.prefs = self.prefs();
        let Screen::Browse(mut b, origin) = std::mem::replace(&mut self.screen, Screen::Closed)
        else {
            return;
        };
        self.screen = match origin {
            Origin::Local => {
                let source = launch::reopened(path.to_local(), &self.settings);
                self.open(source, Origin::Local, Some((b, origin)), ctx)
            }
            Origin::Admin => self.elevate(path.to_local(), Some(Back::Browse(b, origin)), ctx),
            Origin::Remote(ref target) => {
                let target = crate::remote::Target {
                    path: path.to_string(),
                    ..target.clone()
                };
                let conn = crate::remote::Connecting::start(target, &self.settings, ctx);
                Screen::Connecting {
                    conn: Box::new(conn),
                    back: Back::Browse(b, origin),
                }
            }
            Origin::Imported => {
                b.state_mut().status = Some(crate::goto::FROM_FILE.into());
                Screen::Browse(b, origin)
            }
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
                        Screen::Browse(b, _) => b.state_mut().status = Some(problem),
                        _ => eprintln!("dfdu-gui: {problem}"),
                    }
                    return;
                }
            }
        }
        self.prefs.theme = choice;
        let file = self.file_in_use();
        theme::apply(ctx, file);
        let preference = theme::preference(&self.prefs.theme, file);
        ctx.set_theme(preference);
        // The title bar too (Windows, macOS, Wayland): dark with a dark theme.
        ctx.send_viewport_cmd(egui::ViewportCommand::SetTheme(theme::title_bar(
            preference,
        )));
    }

    /// The settings to keep: the theme, and how the browser shows its list.
    pub fn prefs(&self) -> Prefs {
        match &self.screen {
            Screen::Browse(b, _) => Prefs {
                tree: b.state().tree.is_some(),
                details: b.state().details.open,
                ..self.prefs.clone()
            },
            _ => self.prefs.clone(),
        }
    }

    /// No automatic update check, and no notice of a newer release: the window as screenshots
    /// show it.
    pub fn quiet_updates(&mut self) {
        self.updates.quiet = true;
    }

    /// A browser is shown.
    pub fn browsing(&self) -> bool {
        matches!(self.screen, Screen::Browse(..))
    }

    /// The browser shown has finished its scan.
    pub fn scan_done(&self) -> bool {
        match &self.screen {
            Screen::Browse(b, _) => b.state().done_at.is_some() && !b.state().snap.busy,
            _ => false,
        }
    }

    /// The browser shown, if any.
    #[cfg(test)]
    pub fn browser(&self) -> Option<&dyn Browse> {
        match &self.screen {
            Screen::Browse(b, _) => Some(b.as_ref()),
            _ => None,
        }
    }

    /// The browser shown, to change, if any.
    #[cfg(test)]
    pub fn browser_mut(&mut self) -> Option<&mut dyn Browse> {
        match &mut self.screen {
            Screen::Browse(b, _) => Some(b.as_mut()),
            _ => None,
        }
    }

    /// Stops the browser, if one is shown or waits for the scan that replaces it.
    pub fn close(&mut self) {
        match std::mem::replace(&mut self.screen, Screen::Closed) {
            Screen::Browse(b, _)
            | Screen::Elevating {
                back: Some(Back::Browse(b, _)),
                ..
            } => b.shutdown(),
            Screen::Connecting { mut conn, back } => {
                conn.cancel();
                if let Back::Browse(b, _) = back {
                    b.shutdown();
                }
            }
            _ => {}
        }
    }

    /// The screen waiting for an administrator's scan of `path` to start.
    fn elevate(&self, path: PathBuf, back: Option<Back>, ctx: &egui::Context) -> Screen {
        let opened = launch::elevate(path.clone(), &self.settings, ctx);
        Screen::Elevating { path, opened, back }
    }

    /// The browser of `source`, opened as `origin` says, showing its list as the settings
    /// say; the browser `old` it replaces is stopped. When it cannot be opened, `old` shows
    /// again with why, or the screen says why.
    fn open(
        &self,
        source: Source,
        origin: Origin,
        old: Option<(Box<dyn Browse>, Origin)>,
        ctx: &egui::Context,
    ) -> Screen {
        match self.start_browser(source, ctx) {
            Ok(b) => {
                if let Some((old, _)) = old {
                    old.shutdown();
                }
                Screen::Browse(b, origin)
            }
            Err(e) => match old {
                Some((mut old, origin)) => {
                    old.state_mut().status = Some(format!("cannot open it: {e}"));
                    Screen::Browse(old, origin)
                }
                None => Screen::Failed(format!("Cannot open it: {e}")),
            },
        }
    }

    /// The browser of `source`, showing its list as the settings say.
    fn start_browser(&self, source: Source, ctx: &egui::Context) -> io::Result<Box<dyn Browse>> {
        let themes = self.themes().iter().map(|t| t.label().to_owned()).collect();
        let mut b = launch::open(source, &self.settings, ctx)?;
        let s = b.state_mut();
        if self.prefs.tree {
            dfdu_session::tree::act::toggle(s);
        }
        if self.prefs.details {
            dfdu_session::details::toggle(s);
        }
        b.view_mut().themes = themes;
        Ok(b)
    }
}

/// A message alone in the middle of a screen without a browser, in `color`.
fn message(ui: &mut egui::Ui, colors: &Colors, text: &str, color: egui::Color32) {
    egui::CentralPanel::default()
        .frame(start::frame(colors))
        .show(ui, |ui| {
            start::backdrop(ui, colors);
            ui.vertical_centered(|ui| {
                ui.add_space(ui.available_height() * 0.35);
                ui.add(egui::Label::new(kit::sans(text, 13.0).color(color)).wrap());
            });
        });
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
