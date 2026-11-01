//! The start screen, shown without a directory on the command line: the disks as `dfdu --disks`
//! lists them (the data volume first on macOS, disks that do not answer disabled), the system's
//! folder picker (`rfd`), a field to type a folder's path, a folder dropped on the window, and
//! the system's file picker for a scan saved to a file (an export), which is browsed read-only
//! as `--import` browses it.
//!
//! It is drawn on the window's background with a faint grid ([`backdrop`]), in a column at most
//! [`COLUMN`] wide: the heading, the disks as rows of one sheet (a key, the name, mount point
//! and notes, a bar of the use, the free space), then a sheet for a folder and one for an
//! export, then the keys and the button that scans the chosen disk. On macOS the sealed system
//! volume is folded into the data volume of its container: both show the container's space,
//! and the data volume is the one with anything to clean.
//!
//! The arrows choose a disk, Enter scans it, 1 to 9 scan the disk of that number, and Cmd-O
//! (Ctrl-O) opens the folder picker, while no text field has the keyboard.

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
use crate::theme::Colors;
use crate::theme::style::{
    MONO_SEMIBOLD_FAMILY, RADIUS_MD, RADIUS_SM, SPACE_1, SPACE_2, SPACE_3, SPACE_4, SPACE_6,
};

/// The widest the screen's column grows.
const COLUMN: f32 = 760.0;
/// Below this width a row's bar moves under its name, and the two sheets stack.
const WIDE: f32 = 560.0;
/// A disk's row, with its bar beside the name, then under it.
const ROW_WIDE: f32 = 64.0;
const ROW_NARROW: f32 = 80.0;
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
        }
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

    /// The keys of the screen, while no text field has the keyboard: what they chose.
    fn keys(&mut self, ctx: &egui::Context) -> Option<PathBuf> {
        let open = ctx.input_mut(|i| i.consume_key(Modifiers::COMMAND, Key::O));
        if open {
            return rfd::FileDialog::new().pick_folder();
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
                .map(|r| r.disk.mount_point.clone());
        }
        let step = |from: usize, forward: bool| {
            let mut at = from;
            loop {
                at = if forward {
                    at.checked_add(1).filter(|a| *a < rows.len())?
                } else {
                    at.checked_sub(1)?
                };
                if rows[at].enabled() {
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
            return self.chosen_row().map(|r| r.disk.mount_point.clone());
        }
        None
    }

    /// Draws the screen; what was chosen, if anything was.
    pub fn show(&mut self, ui: &mut Ui, colors: &Colors) -> Option<Chosen> {
        if self.rows.is_none()
            && let Ok(listed) = self.listing.try_recv()
        {
            let listed = listed.map(rows);
            if let Ok(rows) = &listed {
                self.selected = rows.iter().position(Row::enabled).unwrap_or(0);
            }
            self.rows = Some(listed);
        }
        let mut chosen = self.keys(ui.ctx());
        let mut export = None;
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
                            let (c, e) = self.column(ui, colors, w);
                            chosen = chosen.take().or(c);
                            export = e;
                        });
                    });
                });
            });
        if let Some(file) = export {
            return Some(Chosen::Export(file));
        }
        if let Some(dropped) = dropped(ui.ctx()) {
            match dropped {
                Ok(dir) => chosen = Some(dir),
                Err(note) => self.dropped = Some(note),
            }
        }
        chosen.map(Chosen::Folder)
    }

    /// The column of the screen, `w` wide: the folder and the export chosen in it.
    fn column(
        &mut self,
        ui: &mut Ui,
        colors: &Colors,
        w: f32,
    ) -> (Option<PathBuf>, Option<PathBuf>) {
        let dim = colors.fg(Token::FgDim);
        let mut chosen = None;
        ui.label(kit::mono_bold("Choose what to scan", 22.0).color(colors.fg(Token::Header)));
        ui.add_space(SPACE_1);
        let what = "dfdu shows what fills it while the scan is still running.";
        ui.label(kit::sans(what, 13.0).color(dim));
        ui.add_space(SPACE_6);

        ui.label(kit::label("Disks", 12.0).color(dim));
        ui.add_space(SPACE_2);
        match &self.rows {
            None => {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label(kit::sans("Listing the disks…", 13.0).color(dim));
                });
            }
            Some(Err(e)) => {
                let text = format!("The disks cannot be listed: {e}");
                ui.label(kit::sans(text, 13.0).color(colors.fg(Token::StatusError)));
            }
            Some(Ok(rows)) if rows.is_empty() => {
                ui.label(kit::sans("No disks found.", 13.0).color(dim));
            }
            Some(Ok(rows)) => {
                let mut clicked = None;
                Frame::new()
                    .fill(colors.chrome.surface)
                    .stroke(Stroke::new(1.0, colors.chrome.divider))
                    .corner_radius(CornerRadius::same(RADIUS_MD))
                    .inner_margin(Margin::same(1))
                    .show(ui, |ui| {
                        ui.spacing_mut().item_spacing.y = 0.0;
                        let inner = w - 4.0;
                        for (i, r) in rows.iter().enumerate() {
                            let last = i + 1 == rows.len();
                            let resp = row(ui, r, i, i == self.selected, last, colors, inner);
                            if resp.clicked() && r.enabled() {
                                clicked = Some(i);
                            }
                        }
                    });
                if let Some(i) = clicked {
                    self.selected = i;
                    chosen = Some(rows[i].disk.mount_point.clone());
                }
            }
        }
        ui.add_space(SPACE_6);

        let two = w >= WIDE;
        let each = if two { (w - SPACE_3) / 2.0 } else { w };
        let mut export = None;
        let mut sheets = |ui: &mut Ui| {
            if let Some(dir) = self.folder_sheet(ui, colors, each) {
                chosen = Some(dir);
            }
            if let Some(file) = export_sheet(ui, colors, each) {
                export = Some(file);
            }
        };
        if two {
            ui.horizontal_top(|ui| {
                ui.spacing_mut().item_spacing.x = SPACE_3;
                sheets(ui);
            });
        } else {
            ui.spacing_mut().item_spacing.y = SPACE_3;
            sheets(ui);
        }
        ui.add_space(SPACE_6);

        if let Some(dir) = self.footer(ui, colors) {
            chosen = Some(dir);
        }
        (chosen, export)
    }

    /// The sheet for a folder, `w` wide: the field, Scan, Browse…, and why a dropped file was
    /// not scanned.
    fn folder_sheet(&mut self, ui: &mut Ui, colors: &Colors, w: f32) -> Option<PathBuf> {
        let mut chosen = None;
        sheet(ui, colors, w, |ui, inner| {
            heading(
                ui,
                colors,
                "A folder",
                "Or drop one anywhere on the window.",
            );
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = SPACE_2;
                let id = egui::Id::unique("dfdu-start-folder");
                let field = (inner - 148.0).max(80.0);
                let r =
                    crate::windows::field(ui, colors, &mut self.folder, id, "~/Projects", field);
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
            if let Some(note) = &self.dropped {
                ui.add_space(SPACE_2);
                let warn = colors.fg(Token::StatusWarn);
                ui.add(egui::Label::new(kit::sans(note, 12.0).color(warn)).wrap());
            }
        });
        chosen
    }

    /// The keys, and the button that scans the chosen disk.
    fn footer(&self, ui: &mut Ui, colors: &Colors) -> Option<PathBuf> {
        let dim = colors.fg(Token::FgDim);
        let open = if cfg!(target_os = "macos") {
            "⌘O"
        } else {
            "Ctrl O"
        };
        let mut chosen = None;
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = SPACE_1;
            let hints: &[(&str, &str)] = if self.listed().iter().any(Row::enabled) {
                &[("↑↓", "choose"), ("Enter", "scan"), (open, "open a folder")]
            } else {
                &[(open, "open a folder")]
            };
            for (n, (key, what)) in hints.iter().enumerate() {
                if n > 0 {
                    ui.add_space(SPACE_3);
                }
                kit::key_cap(ui, colors, key);
                ui.label(kit::sans(*what, 11.0).color(dim));
            }
            if let Some(r) = self.chosen_row() {
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    let text = format!("Scan {}", name(&r.disk));
                    if kit::button(ui, colors, Kind::Primary, &text, None, true).clicked() {
                        chosen = Some(r.disk.mount_point.clone());
                    }
                });
            }
        });
        chosen
    }
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

/// A sheet `w` wide on `surface`, with `add` given its inner width.
fn sheet(ui: &mut Ui, colors: &Colors, w: f32, add: impl FnOnce(&mut Ui, f32)) {
    let pad = SPACE_4;
    Frame::new()
        .fill(colors.chrome.surface)
        .stroke(Stroke::new(1.0, colors.chrome.divider))
        .corner_radius(CornerRadius::same(RADIUS_MD))
        .inner_margin(Margin::same(16))
        .show(ui, |ui| {
            let inner = w - 2.0 * pad - 2.0;
            ui.vertical(|ui| {
                ui.set_width(inner);
                ui.set_min_height(76.0);
                ui.spacing_mut().item_spacing.y = SPACE_1;
                add(ui, inner);
            });
        });
}

/// A sheet's title and the line under it.
fn heading(ui: &mut Ui, colors: &Colors, title: &str, line: &str) {
    ui.label(kit::title(title, 15.0).color(colors.fg(Token::Title)));
    ui.label(kit::sans(line, 12.0).color(colors.fg(Token::FgDim)));
    ui.add_space(SPACE_2);
}

/// The sheet for an export, `w` wide.
fn export_sheet(ui: &mut Ui, colors: &Colors, w: f32) -> Option<PathBuf> {
    let mut file = None;
    sheet(ui, colors, w, |ui, _| {
        let what = "A snapshot or an ncdu export, browsed read-only.";
        heading(ui, colors, "A saved scan", what);
        let open = kit::button(ui, colors, Kind::Secondary, "Open file…", None, true);
        if open.clicked() {
            file = rfd::FileDialog::new().pick_file();
        }
    });
    file
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
