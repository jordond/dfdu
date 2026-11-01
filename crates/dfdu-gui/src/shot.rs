//! `DFDU_GUI_SCREENSHOT=FILE`: a hidden switch for checking the window by eye and in CI. Once
//! the scan is done (or after a few seconds on the start screen), the window writes what it
//! shows to FILE as a binary PPM image and closes. `DFDU_GUI_SCREENSHOT_KEYS` names keys typed
//! once the scan is done, before the picture: each character as typed, a space as `Space`
//! (`i` opens the details panel, a space marks the selected entry, `d` opens the delete dialog).

use std::path::PathBuf;
use std::time::{Duration, Instant};

use crate::app::Gui;

/// The variable that names the file.
pub const VAR: &str = "DFDU_GUI_SCREENSHOT";
/// The variable that names the keys typed first.
pub const KEYS: &str = "DFDU_GUI_SCREENSHOT_KEYS";
/// How long after the scan is done, or the window opened, the picture is taken.
const SETTLE: Duration = Duration::from_secs(1);
/// The longest the window waits for a scan.
const LONGEST: Duration = Duration::from_secs(30);

/// The window, taking a picture of itself into `path`.
pub struct Shot {
    pub gui: Gui,
    pub path: Option<PathBuf>,
    /// The keys still to type.
    pub keys: Option<String>,
    pub opened: Instant,
    /// When the scan was first seen done.
    pub done: Option<Instant>,
}

/// The events that type `keys`.
fn events(keys: &str) -> Vec<egui::Event> {
    keys.chars()
        .map(|c| match c {
            ' ' => egui::Event::Key {
                key: egui::Key::Space,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers: egui::Modifiers::NONE,
            },
            c => egui::Event::Text(c.into()),
        })
        .collect()
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
    }
}

impl eframe::App for Shot {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        if self.done.is_some()
            && let Some(keys) = self.keys.take()
        {
            ctx.input_mut(|i| i.events.extend(events(&keys)));
        }
        self.gui.show(ui);
        if self.path.is_some() {
            self.take(&ctx);
        } else {
            // The picture is read back from the GPU while frames are painted.
            ctx.request_repaint_after(Duration::from_millis(50));
        }
    }

    fn save(&mut self, storage: &mut dyn eframe::Storage) {
        self.gui.prefs().save(storage);
    }

    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        self.gui.close();
    }
}
