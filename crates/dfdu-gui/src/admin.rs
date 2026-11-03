//! The Full disk access window: where scanning as an administrator stands on this system, and
//! the next step to make it work. Opened from the View menus, from the start screen's Set up
//! badge, and when a scan with Full disk access on is asked for before it is ready; checked
//! again every second while open, so a step taken in System Settings shows at once.
//!
//! - Linux: a scan as an administrator asks for the password each time through pkexec (with
//!   dfdu's own polkit action when installed from the .deb); nothing is set up from here. The
//!   window says whether pkexec is there.
//! - macOS: a helper inside the app scans as root (a launch daemon registered with
//!   SMAppService). It is installed from here, approved once in Login Items, and reads the
//!   folders macOS protects only once dfdu has Full Disk Access (the helper shares the app's
//!   grant). The window lists those three steps, each done, current or still to come, with the
//!   current one's button; it removes the helper too. A dfdu run from outside dfdu.app has no
//!   helper: the window says where the app is and how else to scan as root.
//!
//! The window is modal, on the scrim, 520 wide: a title and a line under it, the steps or what
//! stands in the way, then a footer with Remove helper on the left and the button that closes
//! it on the right.

use std::time::{Duration, Instant};

use dfdu_session::palette::Token;
use egui::{
    Align, Color32, CornerRadius, FontFamily, FontId, Frame, Id, Layout, Margin, Modal, RichText,
    Sense, Stroke, StrokeKind, Ui, pos2, vec2,
};

use crate::kit::{self, Kind};
use crate::theme::Colors;
use crate::theme::style::{
    MONO_SEMIBOLD_FAMILY, RADIUS_MD, RADIUS_SM, SPACE_1, SPACE_2, SPACE_3, SPACE_4, SPACE_6,
};

/// How often the window checks again while open.
const RECHECK: Duration = Duration::from_secs(1);
/// The window's width.
const WIDTH: f32 = 520.0;
/// The side of a step's mark.
const MARK: f32 = 28.0;

/// Where scanning as an administrator stands. Linux has only `Unavailable` and `Ready`.
#[derive(Clone, Debug, PartialEq, Eq)]
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub enum Step {
    /// This system offers no way, for the reason given.
    Unavailable(String),
    /// macOS: this dfdu runs outside dfdu.app, which holds the helper.
    OutsideApp,
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
    /// A scan as an administrator can start.
    pub fn usable(&self) -> bool {
        matches!(self, Step::Ready | Step::FullDiskAccess)
    }

    /// The macOS step under way, 0 to 2, or 3 once all are done; `None` off the three steps.
    fn index(&self) -> Option<usize> {
        match self {
            Step::Install => Some(0),
            Step::Approve => Some(1),
            Step::FullDiskAccess => Some(2),
            Step::Ready if cfg!(target_os = "macos") => Some(3),
            _ => None,
        }
    }
}

/// Where things stand now.
pub fn step() -> Step {
    platform::step()
}

/// One of the three macOS steps, as the window lists it.
struct Stage {
    title: &'static str,
    /// What to do, shown while it is the current step.
    what: &'static str,
    /// The step in a line, shown while it is still to come.
    hint: &'static str,
    /// Where in System Settings, shown in mono under `what`.
    place: Option<&'static str>,
    button: &'static str,
    action: Action,
    /// The step finishes in System Settings, and the window moves on by itself.
    waits: bool,
    /// Scans work without it: the step can be skipped.
    optional: bool,
}

const STAGES: [Stage; 3] = [
    Stage {
        title: "Install the helper",
        what: "A small background task that comes with dfdu. It runs only while a scan asks \
               for it.",
        hint: "A small background task that comes with dfdu.",
        place: None,
        button: "Install helper",
        action: Action::Install,
        waits: false,
        optional: false,
    },
    Stage {
        title: "Allow it in System Settings",
        what: "macOS asks you to allow new background tasks once. Turn dfdu on under Allow in \
               the Background.",
        hint: "One switch in Login Items & Extensions.",
        place: Some("General › Login Items & Extensions"),
        button: "Open Login Items",
        action: Action::LoginItems,
        waits: true,
        optional: false,
    },
    Stage {
        title: "Turn on Full Disk Access in macOS",
        what: "Without it, macOS hides Mail, Messages, Safari, Desktop, Documents and a few \
               other folders. Scans still work, leaving those out.",
        hint: "So the scan also counts Mail, Messages, Desktop, Documents and more.",
        place: Some("Privacy & Security › Full Disk Access"),
        button: "Open Full Disk Access",
        action: Action::FullDiskAccess,
        waits: true,
        optional: true,
    },
];

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
        let frame = Frame::window(&ctx.global_style()).inner_margin(Margin::ZERO);
        let modal = Modal::new(Id::unique("dfdu-full-disk-access"))
            .backdrop_color(colors.chrome.scrim)
            .frame(frame)
            .show(ctx, |ui| {
                ui.set_width(WIDTH);
                ui.spacing_mut().item_spacing.y = 0.0;
                if self.step == Step::OutsideApp {
                    self.outside_app(ui, colors, &mut open);
                } else {
                    self.setup(ui, colors, &mut open);
                }
            });
        if modal.should_close() {
            open = false;
        }
        open
    }

    /// The steps, or what stands in the way, then the footer.
    fn setup(&mut self, ui: &mut Ui, colors: &Colors, open: &mut bool) {
        header(
            ui,
            colors,
            "Full disk access",
            "Lets dfdu count the folders your account can't open.",
            open,
        );
        let at = self.step.index();
        let ready = self.step == Step::Ready;
        Frame::new()
            .inner_margin(Margin {
                left: 24,
                right: 24,
                top: 0,
                bottom: 8,
            })
            .show(ui, |ui| {
                kit::rule(ui, colors.chrome.divider, 0.0);
                ui.add_space(SPACE_4);
                if ready {
                    let text = if at.is_some() {
                        " Turn on Full disk access on the start screen to count every folder."
                    } else {
                        " dfdu asks for an administrator's password before each scan."
                    };
                    banner(ui, colors, text);
                    ui.add_space(SPACE_4);
                }
                match (&self.step, at) {
                    (Step::Unavailable(why), _) => {
                        copy(ui, why, colors.fg(Token::Fg), 13.0);
                        ui.add_space(SPACE_3);
                    }
                    (_, Some(at)) => {
                        for (i, stage) in STAGES.iter().enumerate() {
                            if let Some(act) = stage_row(ui, colors, i, stage, at) {
                                match act {
                                    Some(act) => self.act(act),
                                    None => *open = false,
                                }
                            }
                        }
                    }
                    _ => {}
                }
                if let Some(problem) = &self.problem {
                    copy(ui, problem, colors.fg(Token::StatusError), 12.0);
                    ui.add_space(SPACE_3);
                }
            });
        let removable = platform::removable(&self.step);
        let install = self.step == Step::Install;
        let close = if ready {
            "Done"
        } else if at.is_some() {
            "Not now"
        } else {
            "Close"
        };
        footer(ui, colors, |ui| {
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if kit::button(ui, colors, Kind::Secondary, close, None, true).clicked() {
                    *open = false;
                }
                ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
                    if removable {
                        let remove = egui::Button::new(
                            kit::label("Remove helper", 12.0).color(colors.fg(Token::StatusError)),
                        )
                        .frame(false);
                        if ui.add(remove).clicked() {
                            self.act(Action::Remove);
                        }
                    } else if install {
                        ui.label(
                            kit::sans("Needs an administrator of this Mac.", 12.0)
                                .color(colors.fg(Token::FgDim)),
                        );
                    }
                });
            });
        });
    }

    /// A dfdu run from outside dfdu.app: where it runs, where the app is, and the two ways on.
    fn outside_app(&mut self, ui: &mut Ui, colors: &Colors, open: &mut bool) {
        header(
            ui,
            colors,
            "Open dfdu from dfdu.app",
            "Full disk access works only in the dfdu app. This copy runs outside it.",
            open,
        );
        let app = platform::installed_app();
        Frame::new()
            .inner_margin(Margin {
                left: 24,
                right: 24,
                top: 0,
                bottom: 8,
            })
            .show(ui, |ui| {
                Frame::new()
                    .stroke(Stroke::new(1.0, colors.chrome.divider))
                    .corner_radius(CornerRadius::same(RADIUS_MD))
                    .inner_margin(Margin::symmetric(14, 10))
                    .show(ui, |ui| {
                        ui.set_width(ui.available_width());
                        ui.spacing_mut().item_spacing.y = SPACE_2;
                        let exe = std::env::current_exe()
                            .map_or_else(|_| "unknown".into(), |p| p.display().to_string());
                        place_row(ui, colors, "Running from", &exe);
                        let there = app.as_ref().map_or_else(
                            || "not in Applications".into(),
                            |a| a.display().to_string(),
                        );
                        place_row(ui, colors, "The app", &there);
                    });
                ui.add_space(SPACE_6);
                ui.label(kit::title("Two ways on", 14.0).color(colors.fg(Token::Title)));
                ui.add_space(SPACE_3);
                let fg = colors.fg(Token::Fg);
                ui.horizontal_top(|ui| {
                    ui.label(kit::sans("1.", 13.0).color(fg));
                    copy(
                        ui,
                        "Quit this copy and open dfdu from Applications. Set up full disk access \
                         from there.",
                        fg,
                        13.0,
                    );
                });
                ui.add_space(SPACE_2 + 2.0);
                ui.horizontal(|ui| {
                    ui.label(kit::sans("2.", 13.0).color(fg));
                    ui.label(kit::sans("Or scan from a terminal:", 13.0).color(fg));
                    code(ui, colors, "sudo dfdu /");
                });
                if let Some(problem) = &self.problem {
                    ui.add_space(SPACE_3);
                    copy(ui, problem, colors.fg(Token::StatusError), 12.0);
                }
                ui.add_space(SPACE_4);
            });
        footer(ui, colors, |ui| {
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if app.is_some()
                    && kit::button(
                        ui,
                        colors,
                        Kind::Primary,
                        "Show dfdu.app in Finder",
                        None,
                        true,
                    )
                    .clicked()
                {
                    self.act(Action::Reveal);
                }
                if kit::button(ui, colors, Kind::Secondary, "Close", None, true).clicked() {
                    *open = false;
                }
            });
        });
    }

    fn act(&mut self, act: Action) {
        self.problem = platform::act(act).err();
        self.step = step();
        self.checked = Instant::now();
    }
}

/// Wrapped copy in `color`.
pub(crate) fn copy(ui: &mut Ui, text: &str, color: Color32, size: f32) {
    ui.add(egui::Label::new(kit::sans(text, size).color(color)).wrap());
}

/// The window's top: `title` and `line` under it, and a close button on the right.
pub(crate) fn header(ui: &mut Ui, colors: &Colors, title: &str, line: &str, open: &mut bool) {
    Frame::new()
        .inner_margin(Margin {
            left: 24,
            right: 16,
            top: 22,
            bottom: 16,
        })
        .show(ui, |ui| {
            ui.horizontal_top(|ui| {
                let room = ui.available_width() - 32.0 - SPACE_3;
                ui.vertical(|ui| {
                    ui.set_width(room);
                    ui.spacing_mut().item_spacing.y = SPACE_1;
                    ui.label(kit::title(title, 18.0).color(colors.fg(Token::Title)));
                    copy(ui, line, colors.fg(Token::FgDim), 13.0);
                });
                if close_button(ui, colors).clicked() {
                    *open = false;
                }
            });
        });
}

/// A 32 square button with a cross, named Close.
fn close_button(ui: &mut Ui, colors: &Colors) -> egui::Response {
    let (rect, r) = ui.allocate_exact_size(vec2(32.0, 32.0), Sense::click());
    r.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, true, "Close"));
    let p = ui.painter();
    if r.hovered() {
        p.rect_filled(rect, CornerRadius::same(RADIUS_MD), colors.chrome.hover);
    }
    if r.has_focus() {
        p.rect_stroke(
            rect,
            CornerRadius::same(RADIUS_MD),
            Stroke::new(2.0, colors.fg(Token::BorderFocus)),
            StrokeKind::Inside,
        );
    }
    let c = rect.center();
    let stroke = Stroke::new(1.5, colors.fg(Token::FgDim));
    p.line_segment([c + vec2(-4.5, -4.5), c + vec2(4.5, 4.5)], stroke);
    p.line_segment([c + vec2(4.5, -4.5), c + vec2(-4.5, 4.5)], stroke);
    r
}

/// The window's bottom: a `divider` rule, then `add` on `surface.hover` with 24 of room.
pub(crate) fn footer(ui: &mut Ui, colors: &Colors, add: impl FnOnce(&mut Ui)) {
    ui.add_space(SPACE_2);
    let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), 1.0), Sense::hover());
    ui.painter().hline(
        rect.x_range(),
        rect.center().y,
        Stroke::new(1.0, colors.chrome.divider),
    );
    Frame::new()
        .fill(colors.chrome.hover)
        .corner_radius(CornerRadius {
            nw: 0,
            ne: 0,
            sw: 8,
            se: 8,
        })
        .inner_margin(Margin::symmetric(24, 12))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.set_min_height(32.0);
            add(ui);
        });
}

/// The Ready line, outlined in `status.ok`.
fn banner(ui: &mut Ui, colors: &Colors, rest: &str) {
    let ok = colors.fg(Token::StatusOk);
    Frame::new()
        .stroke(Stroke::new(1.0, ok))
        .corner_radius(CornerRadius::same(RADIUS_MD))
        .inner_margin(Margin::symmetric(14, 10))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            let mut job = egui::text::LayoutJob::default();
            let sans = |size| FontId::new(size, FontFamily::Proportional);
            kit::title("Ready.", 13.0)
                .color(colors.fg(Token::Title))
                .append_to(
                    &mut job,
                    ui.style(),
                    egui::FontSelection::Default,
                    Align::Center,
                );
            RichText::new(rest)
                .font(sans(13.0))
                .color(colors.fg(Token::Fg))
                .append_to(
                    &mut job,
                    ui.style(),
                    egui::FontSelection::Default,
                    Align::Center,
                );
            ui.add(egui::Label::new(job).wrap());
        });
}

/// `text` in mono on the window's background, as a command to type.
pub(crate) fn code(ui: &mut Ui, colors: &Colors, text: &str) {
    Frame::new()
        .fill(colors.chrome.bg)
        .corner_radius(CornerRadius::same(RADIUS_SM))
        .inner_margin(Margin::symmetric(6, 2))
        .show(ui, |ui| {
            ui.label(kit::mono(text, 12.0).color(colors.fg(Token::Fg)));
        });
}

/// A row of the outside-the-app table: `name` in `fg.dim`, then `value` in mono.
fn place_row(ui: &mut Ui, colors: &Colors, name: &str, value: &str) {
    ui.horizontal_top(|ui| {
        let (rect, _) = ui.allocate_exact_size(vec2(110.0, 16.0), Sense::hover());
        ui.painter().text(
            rect.left_center(),
            egui::Align2::LEFT_CENTER,
            name,
            FontId::new(12.0, FontFamily::Proportional),
            colors.fg(Token::FgDim),
        );
        ui.add(egui::Label::new(kit::mono(value, 12.0).color(colors.fg(Token::Fg))).wrap());
    });
}

/// Step `i` of the three, with the step under way at `at`: its mark (a tick once done, its
/// number in the accent while current, outlined while to come), a rail down to the next, and
/// its title; the current one says what to do and has its button. Returns what was clicked:
/// `Some(Some(action))` for the step's button, `Some(None)` for Skip for now.
fn stage_row(
    ui: &mut Ui,
    colors: &Colors,
    i: usize,
    stage: &Stage,
    at: usize,
) -> Option<Option<Action>> {
    let (done, current) = (i < at, i == at);
    let mut clicked = None;
    let row = ui.horizontal_top(|ui| {
        ui.spacing_mut().item_spacing.x = 14.0;
        let (mark, _) = ui.allocate_exact_size(vec2(MARK, MARK), Sense::hover());
        paint_mark(ui, colors, mark, i, done, current);
        ui.vertical(|ui| {
            ui.spacing_mut().item_spacing.y = 6.0;
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = SPACE_2;
                let tone = if done || current {
                    Token::Title
                } else {
                    Token::FgDim
                };
                ui.label(kit::title(stage.title, 14.0).color(colors.fg(tone)));
                if current && stage.optional {
                    kit::badge(ui, "Recommended", colors.fg(Token::FgDim));
                }
                if done {
                    ui.label(kit::sans("Done", 12.0).color(colors.fg(Token::StatusOk)));
                }
            });
            if current {
                copy(ui, stage.what, colors.fg(Token::Fg), 13.0);
                if let Some(place) = stage.place {
                    code(ui, colors, place);
                }
                ui.add_space(SPACE_1);
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = SPACE_3;
                    if kit::button(ui, colors, Kind::Primary, stage.button, None, true).clicked() {
                        clicked = Some(Some(stage.action));
                    }
                    if stage.optional {
                        let skip = egui::Button::new(
                            kit::label("Skip for now", 12.0).color(colors.accent()),
                        )
                        .frame(false);
                        if ui.add(skip).clicked() {
                            clicked = Some(None);
                        }
                    } else if stage.waits {
                        kit::dot(ui, colors.accent());
                        ui.label(
                            kit::sans("Moves on by itself once allowed", 12.0)
                                .color(colors.fg(Token::FgDim)),
                        );
                    }
                });
            } else if !done {
                copy(ui, stage.hint, colors.fg(Token::FgDim), 12.0);
            }
            ui.add_space(if i + 1 < STAGES.len() {
                SPACE_6 - 4.0
            } else {
                SPACE_2
            });
        });
    });
    if i + 1 < STAGES.len() {
        let x = row.response.rect.left() + MARK / 2.0;
        let top = row.response.rect.top() + MARK + SPACE_1;
        let bottom = row.response.rect.bottom() - SPACE_1;
        let color = if done {
            colors.fg(Token::StatusOk)
        } else {
            colors.chrome.divider
        };
        if bottom > top {
            ui.painter().vline(x, top..=bottom, Stroke::new(2.0, color));
        }
    }
    clicked
}

/// Paints step `i`'s mark into `rect`.
fn paint_mark(ui: &Ui, colors: &Colors, rect: egui::Rect, i: usize, done: bool, current: bool) {
    let p = ui.painter();
    let c = rect.center();
    let r = MARK / 2.0;
    let on = colors.chrome.on_accent;
    if done {
        p.circle_filled(c, r, colors.fg(Token::StatusOk));
        let stroke = Stroke::new(2.0, on);
        let a = c + vec2(-5.0, 0.5);
        let b = c + vec2(-1.5, 4.0);
        let d = c + vec2(5.0, -3.5);
        p.line_segment([a, b], stroke);
        p.line_segment([b, d], stroke);
        return;
    }
    let text = if current {
        p.circle_filled(c, r, colors.accent());
        on
    } else {
        p.circle_stroke(c, r - 0.75, Stroke::new(1.5, colors.fg(Token::Border)));
        colors.fg(Token::FgDim)
    };
    p.text(
        pos2(c.x, c.y),
        egui::Align2::CENTER_CENTER,
        (i + 1).to_string(),
        FontId::new(12.0, FontFamily::Name(MONO_SEMIBOLD_FAMILY.into())),
        text,
    );
}

/// What a button of the window does.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    Install,
    LoginItems,
    FullDiskAccess,
    Remove,
    /// Shows dfdu.app in the Finder.
    Reveal,
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

    pub fn installed_app() -> Option<std::path::PathBuf> {
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
                "Only an administrator of this Mac can turn on full disk access. Ask one to \
                 scan, or to make your account an administrator in System Settings › Users & \
                 Groups."
                    .into(),
            );
        }
        match service::status(crate::helper::PLIST) {
            // SMAppService reports a daemon never registered as not found, so the plist in the
            // bundle tells that from an app that is not dfdu.app.
            Status::NotFound if bundled() => Step::Install,
            Status::NotFound => Step::OutsideApp,
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
            Action::Reveal => match installed_app() {
                Some(app) => dfdu_session::file_manager::spawn_open(&app, false),
                None => Err("dfdu.app is not in Applications.".into()),
            },
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

    /// dfdu.app in Applications, where it is.
    pub fn installed_app() -> Option<std::path::PathBuf> {
        let app = std::path::Path::new("/Applications/dfdu.app");
        app.is_dir().then(|| app.to_path_buf())
    }

    /// The app bundle holding this program.
    fn app_path() -> Option<String> {
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

    pub fn installed_app() -> Option<std::path::PathBuf> {
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
