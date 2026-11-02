//! The Administrator access window: where scanning as an administrator stands on this system,
//! and the next step to make it work. Opened from the View menus and from the start screen's
//! "As administrator" tick while it is not ready; checked again every second while open, so
//! a step taken in System Settings shows at once.
//!
//! - Linux: a scan as an administrator asks for the password each time through pkexec;
//!   nothing is installed. The window says whether pkexec is there.
//! - macOS: a helper inside the app scans as root (a launch daemon registered with
//!   SMAppService). It is installed from here, approved once in Login Items, and reads the
//!   folders macOS protects only once dfdu has Full Disk Access (the helper shares the app's
//!   grant). The window walks through those steps and removes the helper.

use std::time::{Duration, Instant};

use dfdu_session::palette::Token;
use egui::{Align2, Ui, vec2};

use crate::kit::{self, Kind};
use crate::theme::Colors;
use crate::theme::style::{SPACE_2, SPACE_3};

/// How often the window checks again while open.
const RECHECK: Duration = Duration::from_secs(1);

/// Where scanning as an administrator stands. Linux has only `Unavailable` and `Ready`.
#[derive(Clone, Debug, PartialEq, Eq)]
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub enum Step {
    /// This system offers no way, for the reason given.
    Unavailable(String),
    /// macOS: the helper is not installed.
    Install,
    /// macOS: installed, waiting for approval in Login Items.
    Approve,
    /// macOS: approved, but without Full Disk Access the helper cannot read the folders macOS
    /// protects. Scans work, without those folders.
    FullDiskAccess,
    /// Scans as an administrator work.
    Ready,
}

impl Step {
    /// The state in a few words, for the line under the start screen's tick.
    pub fn summary(&self) -> &'static str {
        match self {
            Step::Unavailable(_) => "not available here",
            Step::Install | Step::Approve => "needs setting up",
            Step::FullDiskAccess => "ready, without Full Disk Access",
            Step::Ready => "ready",
        }
    }

    /// A scan as an administrator can start.
    pub fn usable(&self) -> bool {
        matches!(self, Step::Ready | Step::FullDiskAccess)
    }
}

/// Where things stand now.
pub fn step() -> Step {
    platform::step()
}

/// The window, while open.
pub struct Panel {
    step: Step,
    checked: Instant,
    /// What the last action could not do.
    problem: Option<String>,
}

impl Panel {
    pub fn new() -> Self {
        Panel {
            step: step(),
            checked: Instant::now(),
            problem: None,
        }
    }

    /// Draws the window over the screen; `false` once it is closed.
    pub fn show(&mut self, ctx: &egui::Context, colors: &Colors) -> bool {
        if self.checked.elapsed() >= RECHECK {
            self.step = step();
            self.checked = Instant::now();
        }
        ctx.request_repaint_after(RECHECK);
        let mut open = true;
        egui::Window::new("Administrator access")
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .anchor(Align2::CENTER_CENTER, vec2(0.0, 0.0))
            .default_width(440.0)
            .show(ctx, |ui| {
                ui.set_width(440.0);
                self.body(ui, colors);
            });
        open
    }

    fn body(&mut self, ui: &mut Ui, colors: &Colors) {
        ui.spacing_mut().item_spacing.y = SPACE_2;
        let dim = colors.fg(Token::FgDim);
        let text = |ui: &mut Ui, s: &str| {
            ui.add(egui::Label::new(kit::sans(s, 13.0)).wrap());
        };
        ui.add(
            egui::Label::new(
                kit::sans(
                    "A scan as an administrator runs as root while this window runs as you, so \
                     folders you cannot read are counted too. Deleting, trash and undo stay yours.",
                    12.0,
                )
                .color(dim),
            )
            .wrap(),
        );
        ui.add_space(SPACE_3);
        let action = match &self.step {
            Step::Unavailable(why) => {
                text(ui, why);
                None
            }
            Step::Install => {
                text(
                    ui,
                    "dfdu needs its helper: a background task inside the app that scans as root \
                     for dfdu alone. It runs only while a scan asks for it.",
                );
                Some(("Install helper", Action::Install))
            }
            Step::Approve => {
                text(
                    ui,
                    "Allow dfdu in System Settings › General › Login Items & Extensions, under \
                     Allow in the Background. This window goes on by itself once it is allowed.",
                );
                Some(("Open Login Items", Action::LoginItems))
            }
            Step::FullDiskAccess => {
                text(
                    ui,
                    "The helper is ready, but macOS keeps Mail, Messages, Safari, Desktop, \
                     Documents and other folders from it until dfdu has Full Disk Access. Add \
                     dfdu in Privacy & Security › Full Disk Access; the helper uses the same \
                     permission. Scans work without it, leaving those folders out.",
                );
                if let Some(app) = platform::app_path() {
                    ui.label(kit::mono(app, 11.0).color(dim));
                }
                Some(("Open Full Disk Access", Action::FullDiskAccess))
            }
            Step::Ready => {
                text(
                    ui,
                    "Ready. Tick As administrator on the start screen to scan as root.",
                );
                None
            }
        };
        if let Some(problem) = &self.problem {
            let error = colors.fg(Token::StatusError);
            ui.add(egui::Label::new(kit::sans(problem, 12.0).color(error)).wrap());
        }
        ui.add_space(SPACE_3);
        ui.horizontal(|ui| {
            if let Some((label, act)) = action
                && kit::button(ui, colors, Kind::Primary, label, None, true).clicked()
            {
                self.act(act);
            }
            if platform::removable(&self.step)
                && kit::button(ui, colors, Kind::Secondary, "Remove helper", None, true).clicked()
            {
                self.act(Action::Remove);
            }
        });
    }

    fn act(&mut self, act: Action) {
        self.problem = platform::act(act).err();
        self.step = step();
        self.checked = Instant::now();
    }
}

/// What a button of the window does.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    Install,
    LoginItems,
    FullDiskAccess,
    Remove,
}

#[cfg(target_os = "linux")]
mod platform {
    use super::{Action, Step};

    pub fn step() -> Step {
        if on_path("pkexec") {
            Step::Ready
        } else {
            Step::Unavailable(
                "pkexec is not installed. Install polkit (the package is polkit, or \
                 policykit-1 on older Debian and Ubuntu), then reopen this window."
                    .into(),
            )
        }
    }

    fn on_path(prog: &str) -> bool {
        std::env::var_os("PATH")
            .is_some_and(|p| std::env::split_paths(&p).any(|d| d.join(prog).is_file()))
    }

    pub fn act(_: Action) -> Result<(), String> {
        Ok(())
    }

    pub fn removable(_: &Step) -> bool {
        false
    }

    pub fn app_path() -> Option<String> {
        None
    }
}

#[cfg(target_os = "macos")]
mod platform {
    use super::{Action, Step};
    use dfdu_sys::helper::service::{self, Status};

    pub fn step() -> Step {
        if !crate::helper::may_use(dfdu_sys::acting::this_uid()) {
            return Step::Unavailable(
                "Only an administrator of this Mac can scan as root. Ask one to scan, or to \
                 make your account an administrator in System Settings › Users & Groups."
                    .into(),
            );
        }
        match service::status(crate::helper::PLIST) {
            // SMAppService reports a daemon never registered as not found, so the plist in the
            // bundle tells that from an app that is not dfdu.app.
            Status::NotFound if bundled() => Step::Install,
            Status::NotFound => Step::Unavailable(
                "The helper is part of dfdu.app: open dfdu from its app in Applications to \
                 scan as an administrator."
                    .into(),
            ),
            Status::NotRegistered => Step::Install,
            Status::RequiresApproval => Step::Approve,
            Status::Enabled if crate::helper::full_disk_access() => Step::Ready,
            Status::Enabled => Step::FullDiskAccess,
        }
    }

    pub fn act(act: Action) -> Result<(), String> {
        match act {
            Action::Install => service::register(crate::helper::PLIST)
                .map(|_| ())
                .map_err(|e| format!("Cannot install the helper: {e}")),
            Action::LoginItems => {
                service::open_login_items();
                Ok(())
            }
            Action::FullDiskAccess => dfdu_session::file_manager::spawn_open(
                std::path::Path::new(crate::access::SETTINGS),
                true,
            ),
            Action::Remove => service::unregister(crate::helper::PLIST)
                .map_err(|e| format!("Cannot remove the helper: {e}")),
        }
    }

    pub fn removable(step: &Step) -> bool {
        matches!(step, Step::Approve | Step::FullDiskAccess | Step::Ready)
    }

    /// Whether this program runs from an app bundle holding the helper's launch daemon.
    fn bundled() -> bool {
        app_path().is_some_and(|app| {
            std::path::Path::new(&app)
                .join("Contents/Library/LaunchDaemons")
                .join(crate::helper::PLIST)
                .is_file()
        })
    }

    /// The app bundle holding this program, to drag into Full Disk Access.
    pub fn app_path() -> Option<String> {
        let exe = std::env::current_exe().ok()?;
        let app = exe
            .ancestors()
            .find(|a| a.extension().is_some_and(|e| e == "app"))?;
        Some(app.display().to_string())
    }
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
mod platform {
    use super::{Action, Step};

    pub fn step() -> Step {
        Step::Unavailable(
            "On Windows, run dfdu as an administrator (right-click › Run as administrator).".into(),
        )
    }

    pub fn act(_: Action) -> Result<(), String> {
        Ok(())
    }

    pub fn removable(_: &Step) -> bool {
        false
    }

    pub fn app_path() -> Option<String> {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_scan_can_start_once_ready_with_or_without_full_disk_access() {
        assert!(Step::Ready.usable());
        assert!(Step::FullDiskAccess.usable());
        for s in [Step::Install, Step::Approve, Step::Unavailable("x".into())] {
            assert!(!s.usable(), "{s:?}");
        }
    }
}
