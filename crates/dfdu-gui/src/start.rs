//! The start screen, shown without a directory on the command line: the disks as `dfdu --disks`
//! lists them (the data volume first on macOS, disks that do not answer disabled), the system's
//! folder picker (`rfd`), a field to type a folder's path, a folder dropped on the window, and
//! the system's file picker for a scan saved to a file (an export), which is browsed read-only
//! as `--import` browses it.
//!
//! It is drawn on the window's background with a faint grid ([`backdrop`]), 32 in from its
//! edges: the heading, a card per disk (its name, mount point, a bar of its use, used of total
//! and free), then the ways to choose a folder or an export.

use std::io;
use std::path::PathBuf;

use crossbeam_channel::Receiver;
use dfdu_session::format::{human, ratio};
use dfdu_session::palette::Token;
use dfdu_sys::{Disk, FsClass, Role};
use egui::text::{LayoutJob, TextWrapping};
use egui::{
    Align2, Color32, CornerRadius, CursorIcon, FontFamily, FontId, Frame, Margin, Rect, Sense,
    Stroke, StrokeKind, TextFormat, Ui, WidgetInfo, pos2, vec2,
};

use crate::kit::{self, Kind};
use crate::theme::Colors;
use crate::theme::style::{
    MONO_SEMIBOLD_FAMILY, RADIUS_MD, SPACE_2, SPACE_3, SPACE_4, SPACE_6, SPACE_8,
};

/// A disk's card.
const CARD: egui::Vec2 = vec2(268.0, 134.0);
/// Between the lines of the background grid.
const GRID: f32 = 24.0;

/// What the start screen chose.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Chosen {
    /// A folder to scan.
    Folder(PathBuf),
    /// A scan saved to a file, to browse read-only.
    Export(PathBuf),
}

pub struct Start {
    listing: Receiver<io::Result<Vec<Disk>>>,
    /// The disks once listed.
    disks: Option<io::Result<Vec<Disk>>>,
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
        let disks = spawned.err().map(Err);
        Start {
            listing,
            disks,
            folder: String::new(),
            dropped: None,
        }
    }

    /// Draws the screen; what was chosen, if anything was.
    pub fn show(&mut self, ui: &mut Ui, colors: &Colors) -> Option<Chosen> {
        if self.disks.is_none() {
            self.disks = self.listing.try_recv().ok();
        }
        let mut chosen = None;
        let mut export = None;
        let dim = colors.fg(Token::FgDim);
        egui::CentralPanel::default()
            .frame(frame(colors))
            .show(ui, |ui| {
                backdrop(ui, colors);
                ui.label(kit::mono_bold("Choose a disk", 22.0).color(colors.fg(Token::Header)));
                ui.add_space(SPACE_4);
                match &self.disks {
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
                    Some(Ok(disks)) if disks.is_empty() => {
                        ui.label(kit::sans("No disks found.", 13.0).color(dim));
                    }
                    Some(Ok(disks)) => {
                        ui.horizontal_wrapped(|ui| {
                            ui.spacing_mut().item_spacing = vec2(SPACE_3, SPACE_3);
                            for d in disks {
                                if let Some(p) = card(ui, d, colors) {
                                    chosen = Some(p);
                                }
                            }
                        });
                    }
                }
                ui.add_space(SPACE_8);
                ui.label(kit::label("Or a folder", 11.0).color(dim));
                ui.add_space(SPACE_2);
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = SPACE_2;
                    let open = kit::button(ui, colors, Kind::Primary, "Open folder…", None, true);
                    if open.clicked()
                        && let Some(dir) = rfd::FileDialog::new().pick_folder()
                    {
                        chosen = Some(dir);
                    }
                    ui.add_space(SPACE_2);
                    let id = egui::Id::unique("dfdu-start-folder");
                    let hint = "Path of a folder";
                    let r = crate::windows::field(ui, colors, &mut self.folder, id, hint, 280.0);
                    let typed = !self.folder.trim().is_empty();
                    let entered = r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                    let scan = kit::button(ui, colors, Kind::Secondary, "Scan", None, typed);
                    if (scan.clicked() || entered) && typed {
                        chosen = Some(PathBuf::from(self.folder.trim()));
                    }
                });
                ui.add_space(SPACE_2);
                let drop = "or drop a folder anywhere on the window";
                ui.label(kit::sans(drop, 12.0).color(dim));
                if let Some(note) = &self.dropped {
                    ui.label(kit::sans(note, 12.0).color(colors.fg(Token::StatusWarn)));
                }
                ui.add_space(SPACE_6);
                ui.label(kit::label("Or a scan saved to a file", 11.0).color(dim));
                ui.add_space(SPACE_2);
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = SPACE_3;
                    let open = kit::button(ui, colors, Kind::Secondary, "Open export…", None, true);
                    if open.clicked()
                        && let Some(file) = rfd::FileDialog::new().pick_file()
                    {
                        export = Some(file);
                    }
                    let what = "A snapshot or an ncdu export, browsed read-only.";
                    ui.label(kit::sans(what, 12.0).color(dim));
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

/// What a disk is called on its card: macOS's sealed system volume is `System`, the root of
/// another system `Root`; any other disk the last name of its mount point, or the mount point
/// itself (`C:\`).
fn name(d: &Disk) -> String {
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

/// What a disk's card notes of it: its filesystem, a network one, and that the system volume
/// is read-only (its name already says the role).
fn notes(d: &Disk) -> String {
    let mut notes = vec![d.fstype.as_str()];
    if d.class == FsClass::Network {
        notes.push("network");
    }
    if d.role == Some(Role::System) {
        notes.push("read-only");
    }
    notes.retain(|n| !n.is_empty());
    notes.join(" · ")
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
    job.wrap = TextWrapping::truncate_at_width(width);
    ui.painter().layout_job(job)
}

/// A disk's card, the whole of it a button; its mount point when it was clicked. A disk that
/// does not answer is dimmed and says so.
fn card(ui: &mut Ui, d: &Disk, colors: &Colors) -> Option<PathBuf> {
    let enabled = d.space.is_some();
    let sense = if enabled {
        Sense::click()
    } else {
        Sense::hover()
    };
    let (rect, r) = ui.allocate_exact_size(CARD, sense);
    let at = d.mount_point.display().to_string();
    let spoken = match d.space {
        Some(s) => format!(
            "{at}, {} of {} used, {} free",
            human(s.used),
            human(s.total),
            human(s.available)
        ),
        None => format!("{at}, not responding"),
    };
    r.widget_info(|| WidgetInfo::labeled(egui::Role::Button, enabled, &spoken));
    let r = if enabled {
        r.on_hover_cursor(CursorIcon::PointingHand)
    } else {
        r
    };
    let c = &colors.chrome;
    let mut p = ui.painter().clone();
    if !enabled {
        p.multiply_opacity(0.45);
    }
    let radius = CornerRadius::same(RADIUS_MD);
    let fill = if r.hovered() && enabled {
        c.hover
    } else {
        c.raised
    };
    p.rect_filled(rect, radius, fill);
    let (width, edge) = if r.has_focus() {
        (2.0, colors.fg(Token::BorderFocus))
    } else if r.hovered() && enabled {
        (1.0, colors.fg(Token::BorderFocus))
    } else {
        (1.0, c.divider)
    };
    p.rect_stroke(rect, radius, Stroke::new(width, edge), StrokeKind::Inside);

    let inner = rect.shrink(SPACE_4);
    let w = inner.width();
    let dim = colors.fg(Token::FgDim);
    let bold = FontId::new(15.0, FontFamily::Name(MONO_SEMIBOLD_FAMILY.into()));
    let mono = |size| FontId::new(size, FontFamily::Monospace);
    let mut y = inner.top();
    let g = line(ui, &name(d), bold, colors.fg(Token::Header), w);
    p.galley(pos2(inner.left(), y), g, Color32::PLACEHOLDER);
    y += 22.0;
    let g = line(ui, &at, mono(11.0), dim, w);
    p.galley(pos2(inner.left(), y), g, Color32::PLACEHOLDER);
    y += 22.0;
    // The free space beside what is used when both fit, else beside the notes.
    let mut low_right = None;
    match d.space {
        Some(s) => {
            let bar = Rect::from_min_size(pos2(inner.left(), y), vec2(w, 8.0));
            kit::paint_bar(ui, colors, bar, ratio(s.used, s.total.max(1)));
            y += 18.0;
            let used = format!("{} of {}", human(s.used), human(s.total));
            let used = line(ui, &used, mono(12.0), colors.fg(Token::Fg), w);
            let free = format!("{} free", human(s.available));
            let free = line(ui, &free, mono(12.0), dim, w);
            let used_w = used.size().x;
            p.galley(pos2(inner.left(), y), used, Color32::PLACEHOLDER);
            if used_w + SPACE_3 + free.size().x <= w {
                let at = pos2(inner.right() - free.size().x, y);
                p.galley(at, free, Color32::PLACEHOLDER);
            } else {
                low_right = Some(free);
            }
        }
        None => {
            let warn = colors.fg(Token::StatusWarn);
            let font = FontId::new(12.0, FontFamily::Proportional);
            let at = pos2(inner.left(), y);
            p.text(at, Align2::LEFT_TOP, "Not responding", font, warn);
        }
    }
    let mut room = w;
    if let Some(free) = low_right {
        room -= free.size().x + SPACE_3;
        let at = pos2(
            inner.right() - free.size().x,
            inner.bottom() - free.size().y,
        );
        p.galley(at, free, Color32::PLACEHOLDER);
    }
    let notes = notes(d);
    if !notes.is_empty() {
        let font = FontId::new(11.0, FontFamily::Proportional);
        let g = line(ui, &notes, font, dim, room);
        let at = pos2(inner.left(), inner.bottom() - g.size().y - 1.0);
        p.galley(at, g, Color32::PLACEHOLDER);
    }
    r.clicked().then(|| d.mount_point.clone())
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
            class: FsClass::Local,
            space: None,
            role,
            shares_space_with: Vec::new(),
        }
    }

    #[test]
    fn cards_name_the_root_and_the_system_volume_by_what_they_are() {
        let system = disk("/", Some(Role::System));
        assert_eq!(
            (name(&system), notes(&system)),
            ("System".into(), "apfs · read-only".into())
        );
        let data = disk("/System/Volumes/Data", Some(Role::Data));
        assert_eq!((name(&data), notes(&data)), ("Data".into(), "apfs".into()));
        assert_eq!(name(&disk("/", None)), "Root");
        assert_eq!(name(&disk("/mnt/backup", None)), "backup");
    }
}
