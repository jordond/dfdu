//! `DFDU_GUI_SCREENSHOT=FILE`: a hidden switch for checking the window by eye and in CI. Once
//! the scan is done (or after a few seconds on the start screen), the window writes what it
//! shows to FILE as a binary PPM image and closes.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use crate::app::Gui;

/// The variable that names the file.
pub const VAR: &str = "DFDU_GUI_SCREENSHOT";
/// How long after the scan is done, or the window opened, the picture is taken.
const SETTLE: Duration = Duration::from_secs(1);
/// The longest the window waits for a scan.
const LONGEST: Duration = Duration::from_secs(30);

/// The window, taking a picture of itself into `path`.
pub struct Shot {
    pub gui: Gui,
    pub path: Option<PathBuf>,
    pub opened: Instant,
    /// When the scan was first seen done.
    pub done: Option<Instant>,
}

impl Shot {
    fn take(&mut self, ctx: &egui::Context) {
        let now = Instant::now();
        if self.gui.scan_done() {
            self.done.get_or_insert(now);
        }
        let at = match (self.gui.browsing(), self.done) {
            (true, Some(done)) => done + SETTLE,
            (true, None) => self.opened + LONGEST,
            // The start screen lists the disks first.
            (false, _) => self.opened + SETTLE * 3,
        };
        if now < at {
            ctx.request_repaint_after(Duration::from_millis(100));
            return;
        }
        let Some(path) = self.path.take() else { return };
        let close = ctx.clone();
        ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot(
            egui::ScreenshotCallback::new(move |img| {
                let mut out = format!("P6 {} {} 255\n", img.size[0], img.size[1]).into_bytes();
                for c in &img.pixels {
                    out.extend_from_slice(&[c.r(), c.g(), c.b()]);
                }
                if let Err(e) = std::fs::write(&path, out) {
                    eprintln!("dfdu-gui: {}: {e}", path.display());
                }
                close.send_viewport_cmd(egui::ViewportCommand::Close);
            }),
        ));
        ctx.request_repaint();
    }
}

impl eframe::App for Shot {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.gui.show(ui);
        if self.path.is_some() {
            self.take(&ui.ctx().clone());
        }
    }

    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        self.gui.close();
    }
}
