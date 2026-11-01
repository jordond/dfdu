//! The start screen, shown without a directory on the command line: the disks as `dfdu --disks`
//! lists them (the data volume first on macOS, disks that do not answer disabled), the system's
//! folder picker (`rfd`), a field to type a folder's path, a folder dropped on the window, and
//! the system's file picker for a scan saved to a file (an export), which is browsed read-only
//! as `--import` browses it.

use std::io;
use std::path::PathBuf;

use crossbeam_channel::Receiver;
use dfdu_session::format::{human, ratio};
use dfdu_session::palette::Token;
use dfdu_sys::{Disk, FsClass, Role};
use egui::{Button, ProgressBar, TextEdit, Ui};

use crate::theme::Colors;

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
        egui::CentralPanel::default().show(ui, |ui| {
            ui.heading(colors.text("Choose a disk to browse", Token::Title));
            ui.add_space(ui.spacing().item_spacing.y * 2.0);
            match &self.disks {
                None => {
                    ui.horizontal(|ui| {
                        ui.spinner();
                        ui.label(colors.text("Listing the disks…", Token::FgDim));
                    });
                }
                Some(Err(e)) => {
                    ui.label(colors.text(
                        format!("The disks cannot be listed: {e}"),
                        Token::StatusError,
                    ));
                }
                Some(Ok(disks)) if disks.is_empty() => {
                    ui.label(colors.text("No disks found.", Token::FgDim));
                }
                Some(Ok(disks)) => {
                    egui::Grid::new("disks").striped(true).show(ui, |ui| {
                        for d in disks {
                            if let Some(p) = disk(ui, d, colors) {
                                chosen = Some(p);
                            }
                            ui.end_row();
                        }
                    });
                }
            }
            ui.add_space(ui.spacing().item_spacing.y * 4.0);
            ui.label(colors.text("Or a folder:", Token::Header));
            ui.horizontal(|ui| {
                if ui.button("Open folder…").clicked()
                    && let Some(dir) = rfd::FileDialog::new().pick_folder()
                {
                    chosen = Some(dir);
                }
                let r =
                    ui.add(TextEdit::singleline(&mut self.folder).hint_text("Path of a folder"));
                let typed = !self.folder.trim().is_empty();
                let entered = r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                if (ui.add_enabled(typed, Button::new("Scan")).clicked() || entered) && typed {
                    chosen = Some(PathBuf::from(self.folder.trim()));
                }
            });
            ui.label(colors.text("Or drop a folder on the window.", Token::FgDim));
            if let Some(note) = &self.dropped {
                ui.label(colors.text(note, Token::StatusWarn));
            }
            ui.add_space(ui.spacing().item_spacing.y * 4.0);
            ui.label(colors.text("Or a scan saved to a file:", Token::Header));
            ui.horizontal(|ui| {
                if ui.button("Open export…").clicked()
                    && let Some(file) = rfd::FileDialog::new().pick_file()
                {
                    export = Some(file);
                }
                ui.label(colors.text(
                    "A snapshot or an ncdu export, browsed read-only.",
                    Token::FgDim,
                ));
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

/// One disk's row of the grid; its mount point when it was clicked.
fn disk(ui: &mut Ui, d: &Disk, colors: &Colors) -> Option<PathBuf> {
    let at = d.mount_point.display().to_string();
    let clicked = ui
        .add_enabled(d.space.is_some(), Button::new(colors.text(at, Token::Dir)))
        .clicked();
    ui.label(colors.text(&d.fstype, Token::FgDim));
    match d.space {
        Some(s) => {
            let frac = ratio(s.used, s.total.max(1));
            #[expect(clippy::cast_possible_truncation, reason = "a fraction drawn as a bar")]
            let bar = ProgressBar::new(frac as f32)
                .desired_width(160.0)
                .fill(colors.fg(Token::BarFill))
                .text(format!("{:.0}%", frac * 100.0));
            ui.add(bar);
            ui.label(format!("{} of {}", human(s.used), human(s.total)));
            ui.label(colors.text(format!("{} free", human(s.available)), Token::FgDim));
        }
        None => {
            ui.label(colors.text("not responding", Token::StatusWarn));
            ui.label("");
            ui.label("");
        }
    }
    let mut notes = Vec::new();
    if d.class == FsClass::Network {
        notes.push("network");
    }
    if let Some(role) = d.role {
        notes.push(role.label());
    }
    ui.label(colors.text(notes.join(" · "), Token::FgDim));
    clicked.then(|| d.mount_point.clone())
}
