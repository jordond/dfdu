//! The start screen, shown without a directory on the command line: the disks as `dfdu --disks`
//! lists them (the data volume first on macOS, disks that do not answer disabled), the system's
//! folder picker (`rfd`), a field to type a folder's path, a folder dropped on the window, and
//! the system's file picker for a scan saved to a file (an export), which is browsed read-only
//! as `--import` browses it, and other computers to scan over ssh ([`crate::remote`]).
//!
//! It is drawn on the window's background with a faint grid ([`backdrop`]), in a column at most
//! [`COLUMN`] wide, as one set of places: the heading; This computer, a list of the disks (a
//! key, the name, mount point and notes, a bar of the use, the free space) ending in a row for
//! a folder; Other computers, a list of those scanned lately (a prompt, the name and port, the
//! folder there) ending in a row to connect to another; a line that opens a saved scan; then
//! the keys, the Full disk access switch and the button that opens what is chosen. On macOS
//! the sealed system volume is folded into the data volume of its container: both show the
//! container's space, and the data volume is the one with anything to clean.
//!
//! The arrows choose a disk or a computer scanned lately, Enter opens it, 1 to 9 scan the disk
//! of that number, and Cmd-O (Ctrl-O) opens the folder picker, while no text field has the
//! keyboard.

use std::io;
use std::path::PathBuf;

use crossbeam_channel::Receiver;
use dfdu_session::format::{human, ratio};
use dfdu_session::palette::Token;
use dfdu_sys::{Disk, FsClass, Role};
use egui::text::{LayoutJob, TextWrapping};
use egui::{
    Align, Align2, Color32, CornerRadius, CursorIcon, FontFamily, FontId, Frame, Key, Layout,
    Margin, Modifiers, Rect, Response, Sense, Stroke, StrokeKind, TextFormat, Ui, WidgetInfo, pos2,
    vec2,
};

use crate::kit::{self, Kind};
use crate::remote::Target;
use crate::theme::Colors;
use crate::theme::style::{
    MONO_SEMIBOLD_FAMILY, RADIUS_MD, RADIUS_SM, SANS_MEDIUM_FAMILY, SPACE_1, SPACE_2, SPACE_3,
    SPACE_4, SPACE_6,
};

/// The widest the screen's column grows.
const COLUMN: f32 = 760.0;
/// Below this width a row's bar moves under its name, and the two sheets stack.
const WIDE: f32 = 560.0;
/// Below this width the footer's keys go on a line of their own, above the switch and the button.
const FOOTER_ONE_LINE: f32 = 720.0;
/// A disk's row, with its bar beside the name, then under it.
const ROW_WIDE: f32 = 64.0;
const ROW_NARROW: f32 = 80.0;
/// A computer's row.
const COMPUTER_ROW: f32 = 44.0;
/// A row's bar column and free space column.
const BAR: f32 = 200.0;
const FREE: f32 = 112.0;
/// The share of a disk in use from which its free space is a warning.
const NEARLY_FULL: f64 = 0.9;
/// Between the lines of the background grid.
const GRID: f32 = 24.0;
/// The number keys that scan the first disks.
const NUMBERS: [Key; 9] = [
    Key::Num1,
    Key::Num2,
    Key::Num3,
    Key::Num4,
    Key::Num5,
    Key::Num6,
    Key::Num7,
    Key::Num8,
    Key::Num9,
];

/// What the start screen chose.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Chosen {
    /// A folder to scan.
    Folder(PathBuf),
    /// A scan saved to a file, to browse read-only.
    Export(PathBuf),
    /// A folder on another computer.
    Remote(Target),
}

/// A disk as the list shows it.
#[derive(Clone, Debug)]
struct Row {
    disk: Disk,
    /// The data volume of the startup disk, with the system volume folded into it.
    startup: bool,
}

impl Row {
    fn enabled(&self) -> bool {
        self.disk.space.is_some()
    }
}

pub struct Start {
    listing: Receiver<io::Result<Vec<Disk>>>,
    /// The disks once listed.
    rows: Option<io::Result<Vec<Row>>>,
    /// The row the arrows chose.
    selected: usize,
    /// The folder typed.
    folder: String,
    /// Why what was dropped on the window last is not scanned.
    dropped: Option<String>,
    /// Scan what is chosen as an administrator ([`dfdu_session::elevate`]), where the system
    /// offers it.
    pub admin: bool,
    /// Where that stands, checked at most every second ([`crate::admin`]).
    admin_step: (crate::admin::Step, std::time::Instant),
    /// The Full disk access window was asked for, for the window to open.
    pub setup: bool,
    /// What the sheet for another computer has typed.
    remote: RemoteForm,
    /// Why the last connection to another computer failed.
    pub remote_error: Option<String>,
}

/// The fields of the sheet for another computer.
#[derive(Default)]
struct RemoteForm {
    host: String,
    path: String,
    port: String,
    key: String,
    /// The key file and port are shown.
    more: bool,
}

impl RemoteForm {
    /// The computer the fields name, or why they do not.
    fn target(&self) -> Result<Target, String> {
        let host = self.host.trim();
        if host.is_empty() {
            return Err("Type the computer's name or address.".into());
        }
        if host.starts_with('-') {
            return Err("A computer's name does not start with -.".into());
        }
        let port = match self.port.trim() {
            "" => None,
            p => Some(
                p.parse::<u16>()
                    .ok()
                    .filter(|&p| p > 0)
                    .ok_or_else(|| format!("{p} is not a port."))?,
            ),
        };
        let key = self.key.trim();
        Ok(Target {
            host: host.into(),
            port,
            identity: (!key.is_empty()).then(|| PathBuf::from(key)),
            path: self.path.trim().into(),
        })
    }

    fn fill(&mut self, t: &Target) {
        self.host.clone_from(&t.host);
        self.path.clone_from(&t.path);
        self.port = t.port.map(|p| p.to_string()).unwrap_or_default();
        self.key = t
            .identity
            .as_ref()
            .map(|p| p.display().to_string())
            .unwrap_or_default();
        self.more = t.port.is_some() || t.identity.is_some();
    }
}

/// The frame of a screen without a browser: the window's background, 32 in from its edges.
pub fn frame(colors: &Colors) -> Frame {
    Frame::new()
        .fill(colors.chrome.bg)
        .inner_margin(Margin::same(32))
}

/// Paints the faint grid of the screens without a browser over the whole window.
pub fn backdrop(ui: &Ui, colors: &Colors) {
    let rect = ui.ctx().content_rect();
    let p = ui.ctx().layer_painter(ui.layer_id());
    let stroke = Stroke::new(1.0, colors.chrome.grid);
    let mut x = rect.left() + GRID;
    while x < rect.right() {
        p.vline(x.round() + 0.5, rect.y_range(), stroke);
        x += GRID;
    }
    let mut y = rect.top() + GRID;
    while y < rect.bottom() {
        p.hline(rect.x_range(), y.round() + 0.5, stroke);
        y += GRID;
    }
}

impl Start {
    /// Lists the disks on a thread, which asks `ctx` for a frame when done.
    pub fn new(ctx: &egui::Context) -> Self {
        let (tx, listing) = crossbeam_channel::bounded(1);
        let ctx = ctx.clone();
        let spawned = std::thread::Builder::new()
            .name("dfdu-gui-disks".into())
            .spawn(move || {
                let mut disks = dfdu_sys::disks(dfdu_sys::DISK_TIMEOUT);
                if let Ok(d) = &mut disks {
                    d.sort_by_key(|d| d.role != Some(Role::Data));
                }
                let _ = tx.send(disks);
                ctx.request_repaint();
            });
        Start {
            listing,
            rows: spawned.err().map(Err),
            selected: 0,
            folder: String::new(),
            dropped: None,
            admin: false,
            admin_step: (crate::admin::step(), std::time::Instant::now()),
            setup: false,
            remote: RemoteForm::default(),
            remote_error: None,
        }
    }

    /// Fills the row that connects to a computer with `target`, saying why it failed.
    pub fn failed_remote(&mut self, target: &Target, why: String) {
        self.remote.fill(target);
        self.remote_error = (!why.is_empty()).then_some(why);
    }

    /// Where scanning as an administrator stands, checked again after a second.
    pub fn admin_step(&mut self) -> &crate::admin::Step {
        if self.admin_step.1.elapsed() >= std::time::Duration::from_secs(1) {
            self.admin_step = (crate::admin::step(), std::time::Instant::now());
        }
        &self.admin_step.0
    }

    /// The rows listed, or none.
    fn listed(&self) -> &[Row] {
        match &self.rows {
            Some(Ok(rows)) => rows,
            _ => &[],
        }
    }

    /// The row chosen, when it can be scanned.
    fn chosen_row(&self) -> Option<&Row> {
        self.listed().get(self.selected).filter(|r| r.enabled())
    }

    /// The keys of the screen, while no text field has the keyboard: what they chose. The
    /// arrows move through the disks, then the computers scanned lately; Enter opens the one
    /// chosen, and 1 to 9 scan the disk of that number.
    fn keys(&mut self, ctx: &egui::Context, recent: &[Target]) -> Option<Chosen> {
        let open = ctx.input_mut(|i| i.consume_key(Modifiers::COMMAND, Key::O));
        if open {
            return rfd::FileDialog::new().pick_folder().map(Chosen::Folder);
        }
        if ctx.egui_wants_keyboard_input() {
            return None;
        }
        let (down, up, enter, number) = ctx.input_mut(|i| {
            let number = NUMBERS
                .iter()
                .position(|k| i.consume_key(Modifiers::NONE, *k));
            (
                i.consume_key(Modifiers::NONE, Key::ArrowDown),
                i.consume_key(Modifiers::NONE, Key::ArrowUp),
                i.consume_key(Modifiers::NONE, Key::Enter),
                number,
            )
        });
        let rows = self.listed();
        if let Some(n) = number {
            return rows
                .get(n)
                .filter(|r| r.enabled())
                .map(|r| Chosen::Folder(r.disk.mount_point.clone()));
        }
        let disks = rows.len();
        let usable = |at: usize| rows.get(at).map_or(at - disks < recent.len(), Row::enabled);
        let step = |from: usize, forward: bool| {
            let mut at = from;
            loop {
                at = if forward {
                    at.checked_add(1).filter(|a| *a < disks + recent.len())?
                } else {
                    at.checked_sub(1)?
                };
                if usable(at) {
                    return Some(at);
                }
            }
        };
        let moved = if down {
            step(self.selected, true)
        } else if up {
            step(self.selected, false)
        } else {
            None
        };
        if let Some(at) = moved {
            self.selected = at;
        }
        if enter {
            return self.chosen(recent);
        }
        None
    }

    /// What the arrows chose: a disk that can be scanned, or a computer scanned lately.
    fn chosen(&self, recent: &[Target]) -> Option<Chosen> {
        let disks = self.listed().len();
        match self.chosen_row() {
            Some(r) => Some(Chosen::Folder(r.disk.mount_point.clone())),
            None => (self.selected.checked_sub(disks))
                .and_then(|i| recent.get(i))
                .map(|t| Chosen::Remote(t.clone())),
        }
    }

    /// Draws the screen; what was chosen, if anything was.
    pub fn show(&mut self, ui: &mut Ui, colors: &Colors, recent: &[Target]) -> Option<Chosen> {
        if self.rows.is_none()
            && let Ok(listed) = self.listing.try_recv()
        {
            let listed = listed.map(rows);
            if let Ok(rows) = &listed {
                self.selected = rows.iter().position(Row::enabled).unwrap_or(rows.len());
            }
            self.rows = Some(listed);
        }
        let mut chosen = self.keys(ui.ctx(), recent);
        egui::CentralPanel::default()
            .frame(frame(colors))
            .show(ui, |ui| {
                backdrop(ui, colors);
                egui::ScrollArea::vertical().show(ui, |ui| {
                    let room = ui.available_width();
                    let w = room.min(COLUMN);
                    ui.horizontal_top(|ui| {
                        ui.add_space(((room - w) / 2.0).floor());
                        ui.vertical(|ui| {
                            ui.set_width(w);
                            let c = self.column(ui, colors, w, recent);
                            chosen = chosen.take().or(c);
                        });
                    });
                });
            });
        if let Some(dropped) = dropped(ui.ctx()) {
            match dropped {
                Ok(dir) => chosen = Some(Chosen::Folder(dir)),
                Err(note) => self.dropped = Some(note),
            }
        }
        if let Some(Chosen::Remote(_)) = &chosen {
            self.remote_error = None;
        }
        chosen
    }

    /// The column of the screen, `w` wide: the heading, this computer's disks and a folder,
    /// the other computers, a saved scan, and the footer; what was chosen in it.
    fn column(
        &mut self,
        ui: &mut Ui,
        colors: &Colors,
        w: f32,
        recent: &[Target],
    ) -> Option<Chosen> {
        let dim = colors.fg(Token::FgDim);
        let mut chosen = None;
        ui.label(kit::mono_bold("Choose what to scan", 22.0).color(colors.fg(Token::Header)));
        ui.add_space(SPACE_1);
        let what = "dfdu shows what fills it while the scan is still running.";
        ui.label(kit::sans(what, 13.0).color(dim));
        ui.add_space(SPACE_6);

        ui.label(kit::label("This computer", 12.0).color(dim));
        ui.add_space(SPACE_2);
        list(ui, colors, |ui| {
            let inner = w - 4.0;
            match &self.rows {
                None => note_row(ui, colors, inner, "Listing the disks…", Some(dim), true),
                Some(Err(e)) => {
                    let text = format!("The disks cannot be listed: {e}");
                    let red = colors.fg(Token::StatusError);
                    note_row(ui, colors, inner, &text, Some(red), false);
                }
                Some(Ok(rows)) if rows.is_empty() => {
                    note_row(ui, colors, inner, "No disks found.", Some(dim), false);
                }
                Some(Ok(rows)) => {
                    let mut clicked = None;
                    for (i, r) in rows.iter().enumerate() {
                        // The last has no rule: the folder's row draws one above itself.
                        let last = i + 1 == rows.len();
                        let resp = row(ui, r, i, i == self.selected, last, colors, inner);
                        if resp.clicked() && r.enabled() {
                            clicked = Some(i);
                        }
                    }
                    if let Some(i) = clicked {
                        self.selected = i;
                        chosen = Some(Chosen::Folder(rows[i].disk.mount_point.clone()));
                    }
                }
            }
            if let Some(dir) = self.folder_row(ui, colors, inner) {
                chosen = Some(Chosen::Folder(dir));
            }
        });
        ui.add_space(SPACE_6);

        ui.label(kit::label("Other computers", 12.0).color(dim));
        ui.add_space(SPACE_2);
        list(ui, colors, |ui| {
            let inner = w - 4.0;
            let disks = self.listed().len();
            for (i, t) in recent.iter().enumerate() {
                let selected = self.selected == disks + i;
                let last = i + 1 == recent.len();
                if computer_row(ui, t, selected, last, colors, inner).clicked() {
                    self.selected = disks + i;
                    self.remote.fill(t);
                    chosen = Some(Chosen::Remote(t.clone()));
                }
            }
            if let Some(t) = self.connect_row(ui, colors, inner, !recent.is_empty()) {
                chosen = Some(Chosen::Remote(t));
            }
        });
        ui.add_space(SPACE_4);

        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = SPACE_2;
            let open =
                egui::Button::new(kit::sans("Open a saved scan…", 13.0).color(colors.accent()))
                    .frame(false);
            if ui
                .add(open)
                .on_hover_cursor(CursorIcon::PointingHand)
                .clicked()
                && let Some(file) = rfd::FileDialog::new().pick_file()
            {
                chosen = Some(Chosen::Export(file));
            }
            let what = "a snapshot or an ncdu export, browsed read-only";
            ui.label(kit::sans(what, 12.0).color(dim));
        });
        ui.add_space(SPACE_6);

        if let Some(c) = self.footer(ui, colors, recent) {
            chosen = Some(c);
        }
        chosen
    }

    /// The row of a folder, last among this computer's: a field to type it, Scan, Browse…, and
    /// why something dropped on the window was not scanned.
    fn folder_row(&mut self, ui: &mut Ui, colors: &Colors, w: f32) -> Option<PathBuf> {
        let mut chosen = None;
        let dim = colors.fg(Token::FgDim);
        divider(ui, colors, w);
        Frame::new()
            .inner_margin(Margin::symmetric(16, 12))
            .show(ui, |ui| {
                let inner = w - 32.0;
                ui.set_width(inner);
                ui.spacing_mut().item_spacing = vec2(SPACE_2, SPACE_1);
                ui.horizontal(|ui| {
                    let id = egui::Id::unique("dfdu-start-folder");
                    let field = (inner - 156.0).max(80.0);
                    let hint = "a folder, such as ~/Projects";
                    let r = crate::windows::field(ui, colors, &mut self.folder, id, hint, field);
                    let typed = !self.folder.trim().is_empty();
                    let entered = r.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter));
                    let scan = kit::button(ui, colors, Kind::Secondary, "Scan", None, typed);
                    if (scan.clicked() || entered) && typed {
                        chosen = Some(PathBuf::from(self.folder.trim()));
                    }
                    let browse = kit::button(ui, colors, Kind::Secondary, "Browse…", None, true);
                    if browse.clicked()
                        && let Some(dir) = rfd::FileDialog::new().pick_folder()
                    {
                        chosen = Some(dir);
                    }
                });
                match &self.dropped {
                    Some(note) => {
                        let warn = colors.fg(Token::StatusWarn);
                        ui.add(egui::Label::new(kit::sans(note, 12.0).color(warn)).wrap());
                    }
                    None => {
                        let note = "Or drop a folder anywhere on the window.";
                        ui.label(kit::sans(note, 11.0).color(dim));
                    }
                }
            });
        chosen
    }

    /// The row that connects to a computer typed, last among the other computers (under a
    /// rule when `after` rows): its name and the folder there, Connect, the key file and port
    /// when asked for, and why it last failed.
    fn connect_row(&mut self, ui: &mut Ui, colors: &Colors, w: f32, after: bool) -> Option<Target> {
        let mut chosen = None;
        let dim = colors.fg(Token::FgDim);
        if after {
            divider(ui, colors, w);
        }
        Frame::new()
            .inner_margin(Margin::symmetric(16, 12))
            .show(ui, |ui| {
                let inner = w - 32.0;
                ui.set_width(inner);
                ui.spacing_mut().item_spacing = vec2(SPACE_2, SPACE_1);
                let f = &mut self.remote;
                let mut entered = false;
                let button = 84.0;
                let room = inner - button - 2.0 * SPACE_2;
                let host_w = (room * 0.55).floor();
                ui.horizontal(|ui| {
                    let id = egui::Id::unique("dfdu-start-remote-host");
                    let r = crate::windows::field(ui, colors, &mut f.host, id, "user@host", host_w);
                    entered |= r.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter));
                    let id = egui::Id::unique("dfdu-start-remote-path");
                    let hint = "~ or a folder there";
                    let r = crate::windows::field(ui, colors, &mut f.path, id, hint, room - host_w);
                    entered |= r.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter));
                    let typed = !f.host.trim().is_empty();
                    let go = kit::button(ui, colors, Kind::Secondary, "Connect", None, typed);
                    if (go.clicked() || entered) && typed {
                        match f.target() {
                            Ok(t) => chosen = Some(t),
                            Err(e) => self.remote_error = Some(e),
                        }
                    }
                });
                if f.more {
                    ui.add_space(SPACE_1);
                    ui.horizontal(|ui| {
                        let id = egui::Id::unique("dfdu-start-remote-port");
                        let r = crate::windows::field(ui, colors, &mut f.port, id, "port 22", 88.0);
                        entered |= r.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter));
                        let id = egui::Id::unique("dfdu-start-remote-key");
                        let key_w = (room - 88.0 - SPACE_2).max(80.0);
                        let hint = "key file (default: ssh's)";
                        let r = crate::windows::field(ui, colors, &mut f.key, id, hint, key_w);
                        entered |= r.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter));
                        let choose =
                            kit::button(ui, colors, Kind::Secondary, "Choose…", None, true);
                        if choose.clicked() {
                            let mut pick = rfd::FileDialog::new();
                            if let Some(ssh) = dfdu_sys::dirs::home_dir().map(|h| h.join(".ssh")) {
                                pick = pick.set_directory(ssh);
                            }
                            if let Some(file) = pick.pick_file() {
                                f.key = file.display().to_string();
                            }
                        }
                    });
                }
                ui.horizontal(|ui| {
                    ui.label(
                        kit::sans("Over ssh, with your ssh config and keys.", 11.0).color(dim),
                    );
                    let more = if f.more {
                        "Hide key file and port"
                    } else {
                        "Key file and port…"
                    };
                    let link = egui::Button::new(kit::sans(more, 11.0).color(colors.accent()))
                        .frame(false);
                    if ui
                        .add(link)
                        .on_hover_cursor(CursorIcon::PointingHand)
                        .clicked()
                    {
                        f.more = !f.more;
                    }
                });
                if let Some(e) = &self.remote_error {
                    let red = colors.fg(Token::StatusError);
                    ui.add(egui::Label::new(kit::sans(e, 12.0).color(red)).wrap());
                }
            });
        chosen
    }

    /// The keys, and the button that opens what the arrows chose.
    fn footer(&mut self, ui: &mut Ui, colors: &Colors, recent: &[Target]) -> Option<Chosen> {
        let dim = colors.fg(Token::FgDim);
        let open = if cfg!(target_os = "macos") {
            "⌘O"
        } else {
            "Ctrl O"
        };
        let mut chosen = None;
        let choice = self.chosen(recent);
        let hints: &[(&str, &str)] = if choice.is_some() {
            &[("↑↓", "choose"), ("Enter", "open"), (open, "open a folder")]
        } else {
            &[(open, "open a folder")]
        };
        let keys = |ui: &mut Ui| {
            ui.spacing_mut().item_spacing.x = SPACE_1;
            for (n, (key, what)) in hints.iter().enumerate() {
                if n > 0 {
                    ui.add_space(SPACE_3);
                }
                kit::key_cap(ui, colors, key);
                ui.label(kit::sans(*what, 11.0).color(dim));
            }
        };
        // Narrow: the keys on a line of their own, above the switch and the button.
        let narrow = ui.available_width() < FOOTER_ONE_LINE;
        if narrow {
            ui.horizontal(keys);
            ui.add_space(SPACE_3);
        }
        ui.horizontal(|ui| {
            if !narrow {
                keys(ui);
            }
            let disk = self.chosen_row().map(|r| name(&r.disk));
            let step = self.admin_step().clone();
            let (admin, setup) = (&mut self.admin, &mut self.setup);
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                let text = match (&choice, &disk) {
                    (Some(Chosen::Folder(_)), _) if *admin && !step.usable() => {
                        Some("Set up, then scan…".to_owned())
                    }
                    (Some(Chosen::Folder(_)), Some(disk)) => Some(format!("Scan {disk}")),
                    (Some(Chosen::Remote(t)), _) => Some(format!("Connect to {}", t.host)),
                    _ => None,
                };
                if let Some(text) = text {
                    if kit::button(ui, colors, Kind::Primary, &text, None, true).clicked() {
                        chosen = choice.clone();
                    }
                    ui.add_space(SPACE_4);
                }
                if !matches!(step, crate::admin::Step::Unavailable(_)) {
                    full_disk_access(ui, colors, admin, setup, step.usable());
                }
            });
        });
        chosen
    }
}

/// A list of rows on `surface`, as the disks are drawn.
fn list(ui: &mut Ui, colors: &Colors, add: impl FnOnce(&mut Ui)) {
    Frame::new()
        .fill(colors.chrome.surface)
        .stroke(Stroke::new(1.0, colors.chrome.divider))
        .corner_radius(CornerRadius::same(RADIUS_MD))
        .inner_margin(Margin::same(1))
        .show(ui, |ui| {
            ui.spacing_mut().item_spacing.y = 0.0;
            add(ui);
        });
}

/// The rule between two rows of a list `w` wide.
fn divider(ui: &mut Ui, colors: &Colors, w: f32) {
    let (rect, _) = ui.allocate_exact_size(vec2(w, 1.0), Sense::hover());
    ui.painter().hline(
        rect.x_range().shrink(SPACE_4),
        rect.center().y,
        Stroke::new(1.0, colors.chrome.divider),
    );
}

/// A row of a list `w` wide that only says `text`, in `color`, after a spinner when `busy`.
fn note_row(ui: &mut Ui, colors: &Colors, w: f32, text: &str, color: Option<Color32>, busy: bool) {
    let color = color.unwrap_or_else(|| colors.fg(Token::FgDim));
    Frame::new()
        .inner_margin(Margin::symmetric(16, 14))
        .show(ui, |ui| {
            ui.set_width(w - 32.0);
            ui.horizontal(|ui| {
                if busy {
                    ui.spinner();
                }
                ui.add(egui::Label::new(kit::sans(text, 13.0).color(color)).wrap());
            });
        });
}

/// A computer scanned lately, as a row `w` wide, the whole of it a button: a prompt where the
/// disks have their keys, its name and port, the folder there, and Connect → on the right
/// while pointed at or chosen. The `last` has no rule under it.
fn computer_row(
    ui: &mut Ui,
    t: &Target,
    selected: bool,
    last: bool,
    colors: &Colors,
    w: f32,
) -> Response {
    let (rect, resp) = ui.allocate_exact_size(vec2(w, COMPUTER_ROW), Sense::click());
    resp.widget_info(|| WidgetInfo::labeled(egui::Role::Button, true, t.label()));
    let resp = resp.on_hover_cursor(CursorIcon::PointingHand);
    let c = &colors.chrome;
    let p = ui.painter();
    let cursor_bg = colors.bg(Token::Cursor).unwrap_or(c.hover);
    if selected {
        p.rect_filled(rect, CornerRadius::same(RADIUS_SM), cursor_bg);
    } else if resp.hovered() {
        p.rect_filled(rect, CornerRadius::same(RADIUS_SM), c.hover);
    }
    if resp.has_focus() {
        let stroke = Stroke::new(2.0, colors.fg(Token::BorderFocus));
        p.rect_stroke(
            rect.shrink(1.0),
            CornerRadius::same(RADIUS_SM),
            stroke,
            StrokeKind::Inside,
        );
    } else if !selected && !last {
        p.hline(
            rect.x_range().shrink(SPACE_4),
            rect.bottom() - 0.5,
            Stroke::new(1.0, c.divider),
        );
    }
    let pick = |t: Token| {
        if selected {
            colors.fg(Token::Cursor)
        } else {
            colors.fg(t)
        }
    };
    let inner = rect.shrink2(vec2(SPACE_4, 0.0));
    let mid = rect.center().y;
    let bold = FontId::new(14.0, FontFamily::Name(MONO_SEMIBOLD_FAMILY.into()));
    let mono = FontId::new(12.0, FontFamily::Monospace);
    // A prompt in a cap where the disks have their keys, so names line up with theirs.
    let cap = Rect::from_center_size(pos2(inner.left() + 11.0, mid), vec2(22.0, 18.0));
    let edge = if selected {
        colors.fg(Token::Cursor)
    } else {
        colors.fg(Token::Border)
    };
    p.rect_stroke(
        cap,
        CornerRadius::same(RADIUS_SM),
        Stroke::new(1.0, edge),
        StrokeKind::Inside,
    );
    let glyph = FontId::new(10.0, FontFamily::Name(MONO_SEMIBOLD_FAMILY.into()));
    p.text(
        cap.center(),
        Align2::CENTER_CENTER,
        ">_",
        glyph,
        pick(Token::FgDim),
    );
    let x = inner.left() + 22.0 + SPACE_4;
    let go = "Connect →";
    let go_w = 90.0;
    if selected || resp.hovered() {
        let font = FontId::new(12.0, FontFamily::Name(SANS_MEDIUM_FAMILY.into()));
        p.text(
            pos2(inner.right(), mid),
            Align2::RIGHT_CENTER,
            go,
            font,
            colors.accent(),
        );
    }
    let name_w = ((inner.right() - x - go_w) * 0.45).floor();
    let mut job = LayoutJob::default();
    job.append(&t.host, 0.0, TextFormat::simple(bold, pick(Token::Header)));
    if let Some(port) = t.port {
        job.append(
            &format!(":{port}"),
            0.0,
            TextFormat::simple(mono.clone(), pick(Token::FgDim)),
        );
    }
    job.wrap = TextWrapping::truncate_at_width(name_w);
    let g = ui.fonts_mut(|f| f.layout_job(job));
    p.galley(pos2(x, mid - g.size().y / 2.0), g, Color32::PLACEHOLDER);
    let path = if t.path.trim().is_empty() {
        "~"
    } else {
        t.path.as_str()
    };
    let left = x + name_w + SPACE_4;
    let g = line(
        ui,
        path,
        mono,
        pick(Token::FgDim),
        inner.right() - go_w - left,
    );
    p.galley(pos2(left, mid - g.size().y / 2.0), g, Color32::PLACEHOLDER);
    resp
}

/// The Full disk access switch, laid out right to left in the footer: a scan as an administrator
/// ([`crate::admin`]). Until that is set up, a Set up badge beside its name opens the window that
/// sets it up.
fn full_disk_access(ui: &mut Ui, colors: &Colors, on: &mut bool, setup: &mut bool, ready: bool) {
    const NAME: &str = "Full disk access";
    const CAPTION: &str = "Asks for an administrator's password";
    const BADGE: &str = "Set up";
    let name_font = FontId::new(13.0, FontFamily::Name(SANS_MEDIUM_FAMILY.into()));
    let badge_font = FontId::new(11.0, FontFamily::Name(SANS_MEDIUM_FAMILY.into()));
    let caption_font = FontId::new(11.0, FontFamily::Proportional);
    let width = |text: &str, font: &FontId| {
        ui.painter()
            .layout_no_wrap(text.to_owned(), font.clone(), Color32::WHITE)
            .size()
            .x
    };
    let gap = SPACE_2 + 2.0;
    let badge = if ready {
        0.0
    } else {
        6.0 + width(BADGE, &badge_font) + 2.0 * ui.spacing().button_padding.x + 2.0
    };
    let text = (width(NAME, &name_font) + badge).max(width(CAPTION, &caption_font));
    let size = vec2(40.0 + gap + text.ceil() + 1.0, 36.0);
    // Sized up front, so that the group sits by the button in the footer's right-to-left row
    // while its own parts read left to right.
    ui.allocate_ui_with_layout(size, Layout::left_to_right(Align::Center), |ui| {
        ui.spacing_mut().item_spacing.x = gap;
        kit::switch(ui, colors, on, NAME).on_hover_text(
            "Counts the folders your account can't open too. \
             Asks for an administrator's password before each scan.",
        );
        ui.vertical(|ui| {
            ui.spacing_mut().item_spacing.y = 1.0;
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 6.0;
                let name = egui::RichText::new(NAME)
                    .font(name_font)
                    .color(colors.fg(Token::Title));
                if ui
                    .add(egui::Label::new(name).sense(Sense::click()))
                    .clicked()
                {
                    *on = !*on;
                }
                if !ready {
                    let warn = colors.fg(Token::StatusWarn);
                    let b = egui::Button::new(kit::label(BADGE, 11.0).color(warn))
                        .fill(Color32::TRANSPARENT)
                        .stroke(Stroke::new(1.0, warn))
                        .corner_radius(CornerRadius::same(RADIUS_SM))
                        .min_size(vec2(0.0, 18.0));
                    if ui.add(b).on_hover_text("Set up full disk access").clicked() {
                        *setup = true;
                    }
                }
            });
            ui.label(
                egui::RichText::new(CAPTION)
                    .font(caption_font)
                    .color(colors.fg(Token::FgDim)),
            );
        });
    });
}

/// The rows of `disks`: each disk, but a macOS system volume that shares its space with a data
/// volume listed is folded into it, which is marked the startup disk.
fn rows(disks: Vec<Disk>) -> Vec<Row> {
    let data: Vec<PathBuf> = disks
        .iter()
        .filter(|d| d.role == Some(Role::Data))
        .map(|d| d.mount_point.clone())
        .collect();
    let folded: Vec<PathBuf> = disks
        .iter()
        .filter(|d| d.role == Some(Role::System))
        .filter(|d| d.shares_space_with.iter().any(|p| data.contains(p)))
        .map(|d| d.mount_point.clone())
        .collect();
    disks
        .into_iter()
        .filter(|d| !folded.contains(&d.mount_point))
        .map(|disk| {
            let startup = disk.role == Some(Role::Data)
                && disk.shares_space_with.iter().any(|p| folded.contains(p));
            Row { disk, startup }
        })
        .collect()
}

/// The folder dropped on the window in this frame, or why what was dropped is not scanned.
fn dropped(ctx: &egui::Context) -> Option<Result<PathBuf, String>> {
    let path = ctx.input(|i| i.raw.dropped_files.first().map(|f| f.path().to_path_buf()))?;
    Some(if path.is_dir() {
        Ok(path)
    } else {
        Err(format!("{} is not a folder.", path.display()))
    })
}

/// What a disk is called in its row: its label (`Macintosh HD`) when it has one. Without one,
/// macOS's sealed system volume is `System`, the root of another system `Root`; any other disk
/// the last name of its mount point, or the mount point itself (`C:\`).
fn name(d: &Disk) -> String {
    if let Some(label) = d.label.as_deref().filter(|l| !l.is_empty()) {
        return label.into();
    }
    if d.role == Some(Role::System) {
        return "System".into();
    }
    if d.mount_point == std::path::Path::new("/") {
        return "Root".into();
    }
    d.mount_point.file_name().map_or_else(
        || d.mount_point.display().to_string(),
        |n| n.to_string_lossy().into_owned(),
    )
}

/// What a disk's row notes of it: its filesystem, a network one, that it is the startup disk,
/// and that the system volume is read-only. A labelled data volume not folded with its system
/// volume is noted as such: macOS gives it the system volume's label, and this tells their rows
/// apart (without a label, its name says `Data`).
fn notes(d: &Disk, startup: bool) -> String {
    let mut notes = vec![d.fstype.as_str()];
    if d.class == FsClass::Network {
        notes.push("network");
    }
    if startup {
        notes.push("startup disk");
    }
    match d.role {
        Some(Role::System) => notes.push("read-only"),
        Some(Role::Data) if !startup && d.label.as_deref().is_some_and(|l| !l.is_empty()) => {
            notes.push("Data");
        }
        _ => {}
    }
    notes.retain(|n| !n.is_empty());
    notes.join(" · ")
}

/// The line under a disk's free space: `free`, and how full it is once nearly full.
fn free_note(used: u64, total: u64) -> (String, bool) {
    let full = ratio(used, total.max(1));
    if full >= NEARLY_FULL {
        (format!("free · {:.0}% full", full * 100.0), true)
    } else {
        ("free".into(), false)
    }
}

/// `text` in `font` and `color`, on one line, cut with `…` at `width`.
fn line(
    ui: &Ui,
    text: &str,
    font: FontId,
    color: Color32,
    width: f32,
) -> std::sync::Arc<egui::Galley> {
    let mut job = LayoutJob::single_section(text.to_owned(), TextFormat::simple(font, color));
    job.wrap = TextWrapping::truncate_at_width(width.max(0.0));
    ui.painter().layout_job(job)
}

/// A disk's row `w` wide, the whole of it a button: the key that scans it (the first nine),
/// its name, mount point and notes, a bar of its use, and its free space. The selected row is
/// drawn in `cursor`; a disk that does not answer is dimmed and says so.
#[expect(clippy::too_many_lines, reason = "one row, painted top to bottom")]
fn row(
    ui: &mut Ui,
    r: &Row,
    i: usize,
    selected: bool,
    last: bool,
    colors: &Colors,
    w: f32,
) -> Response {
    let d = &r.disk;
    let enabled = r.enabled();
    let wide = w >= WIDE;
    let h = if wide { ROW_WIDE } else { ROW_NARROW };
    let sense = if enabled {
        Sense::click()
    } else {
        Sense::hover()
    };
    let (rect, resp) = ui.allocate_exact_size(vec2(w, h), sense);
    let at = d.mount_point.display().to_string();
    let title = name(d);
    let spoken = match d.space {
        Some(s) => format!(
            "{title}, {at}, {} of {} used, {} free",
            human(s.used),
            human(s.total),
            human(s.available)
        ),
        None => format!("{title}, {at}, not responding"),
    };
    resp.widget_info(|| WidgetInfo::labeled(egui::Role::Button, enabled, &spoken));
    let resp = if enabled {
        resp.on_hover_cursor(CursorIcon::PointingHand)
    } else {
        resp
    };

    let c = &colors.chrome;
    let mut p = ui.painter().clone();
    let cursor_bg = colors.bg(Token::Cursor).unwrap_or(c.hover);
    let selected = selected && enabled;
    if selected {
        p.rect_filled(rect, CornerRadius::same(RADIUS_SM), cursor_bg);
    } else if resp.hovered() && enabled {
        p.rect_filled(rect, CornerRadius::same(RADIUS_SM), c.hover);
    }
    if resp.has_focus() {
        let ring = rect.shrink(1.0);
        let stroke = Stroke::new(2.0, colors.fg(Token::BorderFocus));
        p.rect_stroke(
            ring,
            CornerRadius::same(RADIUS_SM),
            stroke,
            StrokeKind::Inside,
        );
    }
    if !last && !selected {
        let y = rect.bottom() - 0.5;
        p.hline(
            rect.x_range().shrink(SPACE_4),
            y,
            Stroke::new(1.0, c.divider),
        );
    }
    if !enabled {
        p.multiply_opacity(0.45);
    }

    let pick = |t: Token| {
        if selected {
            colors.fg(Token::Cursor)
        } else {
            colors.fg(t)
        }
    };
    let mono = |size| FontId::new(size, FontFamily::Monospace);
    let bold = |size| FontId::new(size, FontFamily::Name(MONO_SEMIBOLD_FAMILY.into()));
    let sans = |size| FontId::new(size, FontFamily::Proportional);
    let inner = rect.shrink2(vec2(SPACE_4, 0.0));
    let top = rect.top() + if wide { 10.0 } else { 12.0 };

    // The key, in a cap.
    let mut x = inner.left();
    if i < NUMBERS.len() {
        let cap = Rect::from_min_size(pos2(x, top + 2.0), vec2(22.0, 18.0));
        let edge = if selected {
            colors.fg(Token::Cursor)
        } else {
            colors.fg(Token::Border)
        };
        p.rect_stroke(
            cap,
            CornerRadius::same(RADIUS_SM),
            Stroke::new(1.0, edge),
            StrokeKind::Inside,
        );
        let n = (i + 1).to_string();
        p.text(
            cap.center(),
            Align2::CENTER_CENTER,
            n,
            bold(11.0),
            pick(Token::Key),
        );
    }
    x += 22.0 + SPACE_4;

    // The free space, on the right.
    let free_left = inner.right() - FREE;
    match d.space {
        Some(s) => {
            let size = human(s.available);
            let at = pos2(inner.right(), top + 2.0);
            p.text(
                at,
                Align2::RIGHT_TOP,
                size,
                bold(13.0),
                pick(Token::SizeBig),
            );
            let (note, warn) = free_note(s.used, s.total);
            let color = if warn {
                colors.fg(Token::StatusWarn)
            } else {
                pick(Token::FgDim)
            };
            let at = pos2(inner.right(), top + 24.0);
            p.text(at, Align2::RIGHT_TOP, note, sans(11.0), color);
        }
        None => {
            let warn = colors.fg(Token::StatusWarn);
            let at = pos2(inner.right(), top + 4.0);
            p.text(at, Align2::RIGHT_TOP, "Not responding", sans(12.0), warn);
        }
    }

    // The bar of the use, beside the name or under it.
    let text_right = if wide {
        free_left - SPACE_4 - BAR - SPACE_4
    } else {
        free_left - SPACE_4
    };
    if let Some(s) = d.space {
        let frac = ratio(s.used, s.total.max(1));
        if wide {
            let left = free_left - SPACE_4 - BAR;
            let bar = Rect::from_min_size(pos2(left, top + 6.0), vec2(BAR, 8.0));
            kit::paint_bar(ui, colors, bar, frac);
            let used = format!("{} of {}", human(s.used), human(s.total));
            let g = line(ui, &used, mono(11.0), pick(Token::FgDim), BAR);
            p.galley(pos2(left, top + 22.0), g, Color32::PLACEHOLDER);
        } else {
            let bar =
                Rect::from_min_size(pos2(x, rect.bottom() - 18.0), vec2(inner.right() - x, 6.0));
            kit::paint_bar(ui, colors, bar, frac);
        }
    }

    // The name, mount point and notes.
    let room = text_right - x;
    let g = line(ui, &title, bold(14.0), pick(Token::Header), room);
    p.galley(pos2(x, top), g, Color32::PLACEHOLDER);
    let g = line(ui, &at, mono(11.0), pick(Token::FgDim), room);
    p.galley(pos2(x, top + 20.0), g, Color32::PLACEHOLDER);
    if wide {
        let notes = notes(d, r.startup);
        let g = line(ui, &notes, sans(11.0), pick(Token::FgDim), room);
        p.galley(pos2(x, top + 36.0), g, Color32::PLACEHOLDER);
    }
    resp
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;

    fn disk(at: &str, role: Option<Role>) -> Disk {
        Disk {
            mount_point: PathBuf::from(at),
            also_at: Vec::new(),
            fstype: "apfs".into(),
            source: String::new(),
            label: None,
            class: FsClass::Local,
            space: None,
            role,
            shares_space_with: Vec::new(),
        }
    }

    #[test]
    fn rows_name_the_root_and_the_system_volume_by_what_they_are() {
        let system = disk("/", Some(Role::System));
        assert_eq!(
            (name(&system), notes(&system, false)),
            ("System".into(), "apfs · read-only".into())
        );
        let data = disk("/System/Volumes/Data", Some(Role::Data));
        assert_eq!(
            (name(&data), notes(&data, false)),
            ("Data".into(), "apfs".into())
        );
        assert_eq!(name(&disk("/", None)), "Root");
        assert_eq!(name(&disk("/mnt/backup", None)), "backup");
    }

    /// A labelled disk goes by its label, and the data volume that shares the system volume's
    /// label is noted as the data volume, or as the startup disk once folded with it.
    #[test]
    fn rows_name_labelled_disks_by_their_labels() {
        let labelled = |d: Disk, label: &str| Disk {
            label: Some(label.into()),
            ..d
        };
        let system = labelled(disk("/", Some(Role::System)), "Macintosh HD");
        assert_eq!(
            (name(&system), notes(&system, false)),
            ("Macintosh HD".into(), "apfs · read-only".into())
        );
        let data = labelled(
            disk("/System/Volumes/Data", Some(Role::Data)),
            "Macintosh HD",
        );
        assert_eq!(
            (name(&data), notes(&data, false)),
            ("Macintosh HD".into(), "apfs · Data".into())
        );
        assert_eq!(notes(&data, true), "apfs · startup disk");
        let usb = labelled(disk("/media/me/sdb1", None), "My Disk");
        assert_eq!(
            (name(&usb), notes(&usb, false)),
            ("My Disk".into(), "apfs".into())
        );
        // An empty label is no label.
        assert_eq!(name(&labelled(disk("/", None), "")), "Root");
    }

    /// The system volume is folded into the data volume it shares a container with; other
    /// volumes of a container, and a system volume alone, keep their rows.
    #[test]
    fn the_system_volume_folds_into_its_data_volume() {
        let sharing = |d: Disk, with: &[&str]| Disk {
            shares_space_with: with.iter().map(PathBuf::from).collect(),
            ..d
        };
        let data = sharing(
            disk("/System/Volumes/Data", Some(Role::Data)),
            &["/", "/Volumes/Work"],
        );
        let system = sharing(
            disk("/", Some(Role::System)),
            &["/System/Volumes/Data", "/Volumes/Work"],
        );
        let work = sharing(disk("/Volumes/Work", None), &["/", "/System/Volumes/Data"]);
        let listed = rows(vec![data, system, work, disk("/mnt/usb", None)]);
        let shown: Vec<_> = listed
            .iter()
            .map(|r| (r.disk.mount_point.display().to_string(), r.startup))
            .collect();
        assert_eq!(
            shown,
            [
                ("/System/Volumes/Data".into(), true),
                ("/Volumes/Work".into(), false),
                ("/mnt/usb".into(), false),
            ]
        );
        let alone = rows(vec![disk("/", Some(Role::System))]);
        assert_eq!(alone.len(), 1);
        assert!(!alone[0].startup);
    }

    #[test]
    fn free_space_warns_once_nearly_full() {
        assert_eq!(free_note(50, 100), ("free".into(), false));
        assert_eq!(free_note(896, 1000), ("free".into(), false));
        assert_eq!(free_note(900, 1000), ("free · 90% full".into(), true));
        assert_eq!(free_note(0, 0), ("free".into(), false));
    }
}
