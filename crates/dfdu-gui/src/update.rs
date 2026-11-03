//! Updates: a check for a newer release at start and once a day while the window is open (off
//! with the window's switch or `DFDU_NO_UPDATE_CHECK`), a notice in the bottom right corner when
//! one is out, and the Updates window, opened from the View menus and the notice. The window
//! checks again, installs the newer release as this copy's package allows ([`install::How`]),
//! and installs the command-line tool of this version.
//!
//! What the check found is kept in the settings (`settings`), so the notice shows at the next
//! start without checking again within the day. Checks, downloads and installs run on threads
//! of their own and ask for a frame when they end.

mod install;

use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use crossbeam_channel::{Receiver, TryRecvError};
use dfdu_session::palette::Token;
use dfdu_update::{Releases, Version};
use egui::{Align, Align2, Frame, Id, Layout, Margin, Modal, Ui, vec2};

use crate::admin::{code, copy, footer, header};
use crate::kit::{self, Kind};
use crate::settings::Prefs;
use crate::theme::Colors;
use crate::theme::style::{SPACE_1, SPACE_2, SPACE_3, SPACE_4, STATUS_BAR};

pub use install::How;

/// How often the automatic check runs.
const DAY: u64 = 24 * 60 * 60;
/// How long after a failed automatic check it is tried again.
const RETRY: Duration = Duration::from_secs(60 * 60);
const WIDTH: f32 = 520.0;
/// Whether this build checks and downloads (feature `updates`).
const DOWNLOADS: bool = cfg!(feature = "updates");

/// Starts `f` in `stage` when this build downloads; otherwise says it does not.
fn downloading<T: Send + 'static>(
    stage: &mut Stage<T>,
    ctx: &egui::Context,
    f: impl FnOnce() -> Result<T, String> + Send + 'static,
) {
    if DOWNLOADS {
        stage.start(ctx, f);
    } else {
        *stage = Stage::Done(Err(
            "this build of dfdu has no downloads; update it the way it was installed".into(),
        ));
    }
}

/// Work on a thread of its own, until it answers.
struct Task<T>(Receiver<T>);

impl<T: Send + 'static> Task<T> {
    fn spawn(ctx: &egui::Context, f: impl FnOnce() -> T + Send + 'static) -> Self {
        let (tx, rx) = crossbeam_channel::bounded(1);
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            let _ = tx.send(f());
            ctx.request_repaint();
        });
        Task(rx)
    }

    /// Its answer once there is one; `Err` when the thread ended without one.
    fn poll(&self) -> Option<Result<T, ()>> {
        match self.0.try_recv() {
            Ok(v) => Some(Ok(v)),
            Err(TryRecvError::Empty) => None,
            Err(TryRecvError::Disconnected) => Some(Err(())),
        }
    }
}

/// Where a task stands.
enum Stage<T> {
    Idle,
    Running(Task<Result<T, String>>),
    Done(Result<T, String>),
}

impl<T: Send + 'static> Stage<T> {
    fn start(
        &mut self,
        ctx: &egui::Context,
        f: impl FnOnce() -> Result<T, String> + Send + 'static,
    ) {
        *self = Stage::Running(Task::spawn(ctx, f));
    }

    fn poll(&mut self) -> Option<&Result<T, String>> {
        if let Stage::Running(t) = self
            && let Some(r) = t.poll()
        {
            *self = Stage::Done(r.unwrap_or_else(|()| Err("it stopped unexpectedly".into())));
            if let Stage::Done(r) = self {
                return Some(r);
            }
        }
        None
    }

    fn running(&self) -> bool {
        matches!(self, Stage::Running(_))
    }
}

/// The updates of the window: the check, the install of a newer GUI, and of the CLI.
pub struct Updates {
    releases: Releases,
    how: How,
    check: Stage<Version>,
    /// No automatic check before this.
    retry_at: Option<Instant>,
    gui: Stage<install::Installed>,
    cli: Stage<install::CliInstalled>,
    /// The CLI found, looked for when the window opens and after an install (on a thread: it
    /// runs `dfdu --version`).
    found: Option<install::Found>,
    finding: Option<Task<Option<install::Found>>>,
    /// The Updates window is open.
    open: bool,
    /// No automatic check and no notice (screenshots).
    pub quiet: bool,
}

impl Updates {
    pub fn new() -> Self {
        // Tests never reach the release server: a closed port answers at once.
        let releases = if cfg!(test) {
            Releases::new("http://127.0.0.1:1")
        } else {
            Releases::from_env()
        };
        Updates {
            releases,
            how: How::detect(),
            check: Stage::Idle,
            retry_at: None,
            gui: Stage::Idle,
            cli: Stage::Idle,
            found: None,
            finding: None,
            open: false,
            quiet: false,
        }
    }

    /// Checks and downloads from `releases` instead (tests).
    #[cfg(test)]
    pub fn with_releases(releases: Releases) -> Self {
        Updates {
            releases,
            ..Updates::new()
        }
    }

    /// Opens the Updates window, checking again unless a check is under way.
    pub fn open(&mut self, ctx: &egui::Context) {
        self.open = true;
        self.find_cli(ctx);
        if !self.check.running() {
            self.start_check(ctx);
        }
    }

    fn find_cli(&mut self, ctx: &egui::Context) {
        self.finding = Some(Task::spawn(ctx, install::find_cli));
    }

    fn start_check(&mut self, ctx: &egui::Context) {
        let releases = self.releases.clone();
        downloading(&mut self.check, ctx, move || {
            releases.latest().map_err(|e| e.to_string())
        });
    }

    /// Whether the automatic check is due at `now`: a day after the last one, or at once when
    /// the last one is in the future (a clock set back, settings edited by hand).
    fn due(prefs: &Prefs, now: u64) -> bool {
        prefs.checked > now || now - prefs.checked >= DAY
    }

    /// Once a frame: starts the automatic check when it is due, and takes in what the tasks
    /// answered. The installer's download closes the window once it has started the installer,
    /// open or not.
    pub fn tick(&mut self, ctx: &egui::Context, prefs: &mut Prefs) {
        let now = unix_now();
        let due = prefs.updates
            && !self.quiet
            && DOWNLOADS
            && !dfdu_update::checks_disabled()
            && Self::due(prefs, now)
            && self.retry_at.is_none_or(|t| Instant::now() >= t);
        if due && !self.check.running() {
            self.retry_at = Some(Instant::now() + RETRY);
            self.start_check(ctx);
        }
        if let Some(Ok(v)) = self.check.poll() {
            prefs.checked = now;
            prefs.latest = Some(v.to_string());
        }
        if let Some(Ok(install::Installed::Close)) = self.gui.poll() {
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }
        if self.cli.poll().is_some() {
            self.find_cli(ctx);
        }
        if let Some(task) = &self.finding
            && let Some(found) = task.poll()
        {
            self.found = found.ok().flatten();
            self.finding = None;
        }
        if prefs.updates && !self.quiet {
            let next = prefs
                .checked
                .saturating_add(DAY)
                .saturating_sub(now)
                .max(60);
            ctx.request_repaint_after(Duration::from_secs(next));
        }
    }

    /// The newer release the settings know of.
    fn newer(prefs: &Prefs) -> Option<Version> {
        let latest = Version::parse(prefs.latest.as_deref()?)?;
        (latest > Version::current()).then_some(latest)
    }

    /// The notice in the bottom right corner while a newer release is out and was not
    /// dismissed, and the window is closed.
    pub fn notice(&mut self, ctx: &egui::Context, colors: &Colors, prefs: &mut Prefs) {
        let Some(v) = Self::newer(prefs) else { return };
        if self.open || self.quiet || prefs.skipped.as_deref() == Some(&v.to_string()) {
            return;
        }
        egui::Area::new(Id::unique("dfdu-update-notice"))
            // Above the status bar, in from the window's edge.
            .anchor(
                Align2::RIGHT_BOTTOM,
                vec2(-SPACE_4, -(STATUS_BAR + SPACE_1)),
            )
            .order(egui::Order::Foreground)
            .show(ctx, |ui| {
                kit::raised_sheet(colors)
                    .inner_margin(Margin::symmetric(14, 10))
                    .show(ui, |ui| {
                        ui.horizontal(|ui| {
                            ui.label(
                                kit::sans(format!("dfdu {v} is available"), 13.0)
                                    .color(colors.fg(Token::Fg)),
                            );
                            ui.add_space(SPACE_2);
                            if kit::button(ui, colors, Kind::Primary, "Update…", None, true)
                                .clicked()
                            {
                                self.open(ctx);
                            }
                            if kit::button(ui, colors, Kind::Secondary, "Not now", None, true)
                                .clicked()
                            {
                                prefs.skipped = Some(v.to_string());
                            }
                        });
                    });
            });
    }

    /// The Updates window, while open.
    pub fn window(&mut self, ctx: &egui::Context, colors: &Colors, prefs: &mut Prefs) {
        if !self.open {
            return;
        }
        let mut open = true;
        let frame = Frame::window(&ctx.global_style()).inner_margin(Margin::ZERO);
        let modal = Modal::new(Id::unique("dfdu-updates"))
            .backdrop_color(colors.chrome.scrim)
            .frame(frame)
            .show(ctx, |ui| {
                ui.set_width(WIDTH);
                ui.spacing_mut().item_spacing.y = 0.0;
                let line = format!("This is dfdu {}.", dfdu_update::CURRENT);
                header(ui, colors, "Updates", &line, &mut open);
                Frame::new()
                    .inner_margin(Margin {
                        left: 24,
                        right: 24,
                        top: 0,
                        bottom: 8,
                    })
                    .show(ui, |ui| {
                        ui.spacing_mut().item_spacing.y = SPACE_2;
                        kit::rule(ui, colors.chrome.divider, 0.0);
                        ui.add_space(SPACE_2);
                        self.release(ui, colors, prefs);
                        ui.add_space(SPACE_2);
                        ui.horizontal(|ui| {
                            kit::switch(ui, colors, &mut prefs.updates, "Check automatically");
                            ui.label(
                                kit::sans("Check for updates automatically", 13.0)
                                    .color(colors.fg(Token::Fg)),
                            );
                        });
                        ui.add_space(SPACE_3);
                        kit::rule(ui, colors.chrome.divider, 0.0);
                        ui.add_space(SPACE_2);
                        self.command_line(ui, colors);
                    });
                footer(ui, colors, |ui| {
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if kit::button(ui, colors, Kind::Secondary, "Close", None, true).clicked() {
                            open = false;
                        }
                    });
                });
            });
        if modal.should_close() {
            open = false;
        }
        self.open = open;
    }

    /// Whether a newer release is out, and what to do about it.
    fn release(&mut self, ui: &mut Ui, colors: &Colors, prefs: &mut Prefs) {
        let title = |ui: &mut Ui, text: &str| {
            ui.label(kit::title(text, 14.0).color(colors.fg(Token::Title)));
        };
        let dim = colors.fg(Token::FgDim);
        match &self.check {
            Stage::Running(_) => {
                ui.horizontal(|ui| {
                    ui.spinner();
                    copy(ui, "Checking for updates…", dim, 13.0);
                });
                return;
            }
            Stage::Done(Err(e)) => {
                title(ui, "Could not check for updates");
                copy(ui, e, colors.fg(Token::StatusError), 12.0);
                if kit::button(ui, colors, Kind::Secondary, "Try again", None, true).clicked() {
                    self.start_check(ui.ctx());
                }
                return;
            }
            Stage::Idle | Stage::Done(Ok(_)) => {}
        }
        let Some(v) = Self::newer(prefs) else {
            title(ui, "dfdu is up to date");
            copy(ui, "You have the latest release.", dim, 13.0);
            return;
        };
        title(ui, &format!("dfdu {v} is available"));
        ui.hyperlink_to(
            kit::sans("Release notes", 13.0).color(colors.accent()),
            self.releases.page(&v),
        );
        match &self.how {
            How::Command(cmd) => {
                copy(ui, "Update it with:", dim, 13.0);
                code(ui, colors, cmd);
            }
            How::Manual => {
                copy(ui, "Download it from the release page.", dim, 13.0);
            }
            how => {
                let how = how.clone();
                match &self.gui {
                    Stage::Running(_) => {
                        ui.horizontal(|ui| {
                            ui.spinner();
                            copy(ui, &format!("Downloading dfdu {v}…"), dim, 13.0);
                        });
                    }
                    Stage::Done(Ok(install::Installed::Restart(program))) => {
                        copy(ui, "Installed. Restart dfdu to use it.", dim, 13.0);
                        if kit::button(ui, colors, Kind::Primary, "Restart", None, true).clicked() {
                            match install::restart(program) {
                                Ok(()) => ui.send_viewport_cmd(egui::ViewportCommand::Close),
                                Err(e) => {
                                    self.gui = Stage::Done(Err(format!("restarting: {e}")));
                                }
                            }
                        }
                    }
                    Stage::Done(Ok(install::Installed::Close)) => {
                        // `tick` closes the window for the installer.
                        copy(ui, "Starting the installer…", dim, 13.0);
                    }
                    stage => {
                        if let Stage::Done(Err(e)) = stage {
                            copy(ui, e, colors.fg(Token::StatusError), 12.0);
                        }
                        let what = if how == How::Setup {
                            "Download and run the installer"
                        } else {
                            "Update"
                        };
                        if kit::button(ui, colors, Kind::Primary, what, None, true).clicked() {
                            let releases = self.releases.clone();
                            downloading(&mut self.gui, ui.ctx(), move || {
                                install::gui(&how, &releases, &v)
                            });
                        }
                    }
                }
            }
        }
    }

    /// The command-line tool: where it is, and the button that installs this version.
    fn command_line(&mut self, ui: &mut Ui, colors: &Colors) {
        let dim = colors.fg(Token::FgDim);
        ui.label(kit::title("Command-line tool", 14.0).color(colors.fg(Token::Title)));
        let Some(dest) = install::cli_dest() else {
            copy(
                ui,
                "There is no dfdu command-line build for this system.",
                dim,
                13.0,
            );
            return;
        };
        match &self.found {
            Some(found) => {
                let what = found.version.as_deref().unwrap_or("dfdu");
                copy(
                    ui,
                    &format!("{what} is at {}.", shown(&found.path)),
                    dim,
                    13.0,
                );
            }
            None => copy(ui, "Not installed.", dim, 13.0),
        }
        let current = dfdu_update::CURRENT;
        let extras = if cfg!(windows) {
            ", added to your PATH"
        } else {
            ", with its man pages and shell completions"
        };
        copy(
            ui,
            &format!("Installs dfdu {current} into {}{extras}.", shown(&dest)),
            dim,
            12.0,
        );
        match &self.cli {
            Stage::Running(_) => {
                ui.horizontal(|ui| {
                    ui.spinner();
                    copy(ui, "Installing…", dim, 13.0);
                });
            }
            stage => {
                match stage {
                    Stage::Done(Ok(done)) => {
                        let with = if done.extras > 0 {
                            ", with its man pages and completions"
                        } else {
                            ""
                        };
                        copy(
                            ui,
                            &format!("Installed {}{with}.", shown(&done.path)),
                            dim,
                            13.0,
                        );
                        if let Some((line, file)) = &done.path_hint {
                            copy(
                                ui,
                                &format!("If your shell does not find dfdu, add this to {file}:"),
                                dim,
                                12.0,
                            );
                            code(ui, colors, line);
                        }
                        if cfg!(windows) {
                            copy(ui, "Open a new terminal to use it.", dim, 12.0);
                        }
                    }
                    Stage::Done(Err(e)) => copy(ui, e, colors.fg(Token::StatusError), 12.0),
                    _ => {}
                }
                let label = match &self.found {
                    Some(f) if f.path == dest => {
                        let same = f.version.as_deref() == Some(&format!("dfdu {current}"));
                        if same { "Reinstall" } else { "Update" }
                    }
                    _ => "Install",
                };
                ui.add_space(SPACE_1);
                if kit::button(ui, colors, Kind::Secondary, label, None, true).clicked() {
                    let releases = self.releases.clone();
                    let v = Version::current();
                    downloading(&mut self.cli, ui.ctx(), move || install::cli(&releases, &v));
                }
            }
        }
        ui.add_space(SPACE_4);
    }
}

/// `path` with the home directory as `~`.
fn shown(path: &std::path::Path) -> String {
    let home = dfdu_sys::dirs::home_dir().filter(|_| cfg!(unix));
    match home.and_then(|h| path.strip_prefix(h).ok().map(std::path::Path::to_path_buf)) {
        Some(rel) => format!("~/{}", rel.display()),
        None => path.display().to_string(),
    }
}

/// Seconds since the Unix epoch.
pub fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

#[cfg(test)]
pub(crate) mod tests {
    use std::io::{BufRead, BufReader, Write};

    use super::*;

    /// Serves `files` (path, body) over HTTP, one request per connection, with `/latest`
    /// redirecting as `latest` says when given; the base URL.
    pub fn serve_releases(latest: Option<&str>, files: Vec<(String, Vec<u8>)>) -> String {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = l.local_addr().unwrap();
        let latest = latest.map(str::to_owned);
        std::thread::spawn(move || {
            for s in l.incoming() {
                let Ok(mut s) = s else { break };
                let mut r = BufReader::new(s.try_clone().unwrap());
                let mut first = String::new();
                let _ = r.read_line(&mut first);
                loop {
                    let mut h = String::new();
                    if r.read_line(&mut h).unwrap_or(0) == 0 || h == "\r\n" {
                        break;
                    }
                }
                let path = first.split(' ').nth(1).unwrap_or_default();
                let (head, body) = match (&latest, files.iter().find(|(p, _)| p == path)) {
                    (Some(tag), _) if path == "/latest" => {
                        (format!("302 Found\r\nLocation: /tag/{tag}"), &[][..])
                    }
                    (_, Some((_, b))) => ("200 OK".to_owned(), b.as_slice()),
                    _ => ("404 Not Found".to_owned(), &[][..]),
                };
                let _ = write!(
                    s,
                    "HTTP/1.1 {head}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                );
                let _ = s.write_all(body);
            }
        });
        format!("http://{addr}")
    }

    /// [`serve_releases`] without a latest release.
    pub fn serve(files: Vec<(String, Vec<u8>)>) -> String {
        serve_releases(None, files)
    }

    /// Asset `name` of release `v` holding `body`, with its checksum.
    pub fn asset(v: &str, name: &str, body: Vec<u8>) -> Vec<(String, Vec<u8>)> {
        let sum = format!("{}  {name}\n", dfdu_update::sha256_hex(&body));
        let path = format!("/download/v{v}/{name}");
        vec![(format!("{path}.sha256"), sum.into_bytes()), (path, body)]
    }

    /// A `.tar.xz` holding `files` (path, bytes).
    #[cfg(unix)]
    pub fn tar_xz(files: &[(&str, &[u8])]) -> Vec<u8> {
        let mut tar = tar::Builder::new(Vec::new());
        for (name, data) in files {
            let mut h = tar::Header::new_gnu();
            h.set_size(data.len() as u64);
            h.set_mode(0o755);
            h.set_cksum();
            tar.append_data(&mut h, name, *data).unwrap();
        }
        let tar = tar.into_inner().unwrap();
        let mut xz =
            lzma_rust2::XzWriter::new(Vec::new(), lzma_rust2::XzOptions::with_preset(1)).unwrap();
        xz.write_all(&tar).unwrap();
        xz.finish().unwrap()
    }

    #[test]
    fn the_check_is_due_daily_and_after_a_clock_set_back() {
        let at = |checked| Prefs {
            checked,
            ..Prefs::default()
        };
        let now = 1_800_000_000;
        assert!(Updates::due(&at(0), now));
        assert!(!Updates::due(&at(now), now));
        assert!(!Updates::due(&at(now - DAY + 1), now));
        assert!(Updates::due(&at(now - DAY), now));
        // A check recorded in the future is due at once, and nothing overflows.
        assert!(Updates::due(&at(now + 1), now));
        assert!(Updates::due(&at(u64::MAX), now));
    }

    /// Frames until `done`, as the window runs them while its tasks are under way.
    fn ticks(u: &mut Updates, prefs: &mut Prefs, done: impl Fn(&Updates, &Prefs) -> bool) {
        let ctx = egui::Context::default();
        let deadline = Instant::now() + Duration::from_secs(30);
        while !done(u, prefs) {
            assert!(Instant::now() < deadline, "waited too long");
            u.tick(&ctx, prefs);
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    #[test]
    fn a_due_check_records_the_latest_release() {
        let base = serve_releases(Some("v99.1.0"), Vec::new());
        let mut u = Updates::with_releases(Releases::new(&base));
        let mut prefs = Prefs::default();
        ticks(&mut u, &mut prefs, |_, p| p.latest.is_some());
        assert_eq!(prefs.latest.as_deref(), Some("99.1.0"));
        assert!(prefs.checked > 0);
        assert_eq!(
            Updates::newer(&prefs).map(|v| v.to_string()).as_deref(),
            Some("99.1.0")
        );
    }

    #[test]
    fn a_failed_check_waits_before_trying_again() {
        let mut u = Updates::with_releases(Releases::new(&serve(Vec::new())));
        let mut prefs = Prefs::default();
        ticks(&mut u, &mut prefs, |u, _| {
            matches!(u.check, Stage::Done(Err(_)))
        });
        assert_eq!(prefs.checked, 0);
        u.tick(&egui::Context::default(), &mut prefs);
        assert!(!u.check.running(), "tried again at once");
    }

    #[test]
    fn no_check_when_turned_off_or_quiet() {
        let base = serve_releases(Some("v99.1.0"), Vec::new());
        let ctx = egui::Context::default();
        let mut prefs = Prefs {
            updates: false,
            ..Prefs::default()
        };
        let mut u = Updates::with_releases(Releases::new(&base));
        u.tick(&ctx, &mut prefs);
        assert!(matches!(u.check, Stage::Idle));
        prefs.updates = true;
        u.quiet = true;
        u.tick(&ctx, &mut prefs);
        assert!(matches!(u.check, Stage::Idle));
    }
}
