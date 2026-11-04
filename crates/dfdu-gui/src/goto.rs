//! Go to folder (Cmd-L or Cmd-Shift-G, Ctrl-L or Ctrl-Shift-G elsewhere, the registry's
//! `Ctrl-G` and the command palette): a path typed in a field that starts with the directory
//! shown. `Tab` completes its last component from the directories on the scanned computer
//! (another computer's are asked of its agent and listed when they arrive, [`poll`]), and when
//! several match they are listed below the field, each one a link that completes to it. With
//! nothing or only the root typed, the computer's mounts are listed instead, each a link that
//! puts it in the field. `Enter` goes there, in the scan when it is inside it and as
//! a new scan otherwise; what is wrong with the path is shown below the field, which keeps the
//! keyboard. `Esc` closes it. How paths resolve is in `dfdu_session::goto`.

use dfdu_session::SessionState;
use dfdu_session::goto::{self as session, Completion, Gone, Mount, Offer};
use dfdu_session::palette::Token;
use dfdu_tree::Host;
use egui::text::{CCursor, CCursorRange};
use egui::{Align2, Color32, EventFilter, Frame, Id, Key, Margin, Modal, Modifiers, vec2};

use crate::browser::{Front, Intent, ViewState};
use crate::kit;
use crate::theme::Colors;
use crate::theme::style::SPACE_1;

/// The id of the path field.
const FIELD: &str = "dfdu-goto";
/// The window's width.
const WIDTH: f32 = 560.0;
/// Directory names listed at most after a `Tab` that found several.
const SHOWN: usize = 8;
/// Mounts listed at most.
const MOUNTS_SHOWN: usize = 12;

/// What the window says of a scan read from a file when something outside it is asked for
/// ([`SessionState::from_file`]): a scan read from a file is left from the start screen, not
/// by running dfdu again.
pub const FROM_FILE: &str = "this scan was read from a file: open a folder from the start screen";

/// The open window.
#[derive(Debug, Default)]
pub struct GoTo {
    pub text: String,
    /// The names the last `Tab` found, when it found several.
    pub candidates: Vec<String>,
    /// The computer's mounts, while nothing or only the root is typed.
    pub mounts: Vec<Mount>,
    /// The computer was asked and has not answered yet.
    pub asking: bool,
    /// What is wrong with the path the last `Enter` tried, or that `Tab` found nothing.
    pub error: Option<String>,
    /// The field gets the keyboard, with the cursor at the end, in the next frame.
    pub focus: bool,
}

impl GoTo {
    /// The window for the browser `s`, with the directory shown.
    pub fn new(s: &SessionState) -> Self {
        GoTo {
            text: session::prefill(s),
            focus: true,
            ..GoTo::default()
        }
    }

    /// Takes in what the computer offers for the text: what a `Tab` completed, its mounts,
    /// or why it could not say.
    pub fn offered(&mut self, offer: Offer) {
        self.asking = false;
        self.mounts.clear();
        match offer {
            Offer::Completed(c) => self.completed(c),
            Offer::Mounts(m) => {
                self.candidates.clear();
                self.error = None;
                self.mounts = m;
            }
            Offer::Failed(why) => self.failed(why),
        }
    }

    /// Takes in what a `Tab` found: the text completed as far as it goes, the names it can
    /// still become, or that nothing starts with what was typed.
    pub fn completed(&mut self, c: Completion) {
        if let Some(text) = &c.text {
            self.text.clone_from(text);
        }
        self.error = (c.text.is_none() && c.candidates.is_empty())
            .then(|| "no folder here starts with that".to_owned());
        self.candidates = c.candidates;
        self.focus = true;
    }

    /// Completes the text to `name`, one of the candidates: the folder below the one typed.
    pub fn choose(&mut self, name: &str, host: Host) {
        self.text = with_name(&self.text, name, host);
        self.edited();
        self.focus = true;
    }

    /// Puts the mount `m`, one of those listed, in the field.
    pub fn choose_mount(&mut self, m: &Mount, host: Host) {
        self.text = m.text(host);
        self.edited();
        self.focus = true;
    }

    /// Clears what was shown for the text before it changed.
    fn edited(&mut self) {
        self.candidates.clear();
        self.mounts.clear();
        self.asking = false;
        self.error = None;
    }

    /// Takes in what `Enter` found wrong with the path; the field keeps the keyboard.
    pub fn failed(&mut self, why: String) {
        self.error = Some(why);
        self.candidates.clear();
        self.focus = true;
    }
}

/// Takes in where `Enter` went ([`dfdu_session::Session::go_to_path`]). The window closes on a
/// folder of the scan, which the list scrolls to; on a path outside it, which the window
/// scans; and on a path inside it where the tree has no folder, which the status line
/// explains. What is wrong with the path keeps it open.
pub fn gone_to(front: &mut Front, gone: Result<Gone, String>) {
    let view = &mut front.view;
    match gone {
        Ok(Gone::Within(_)) => {
            view.goto = None;
            view.follow = true;
        }
        Ok(Gone::Outside(path)) => {
            view.goto = None;
            view.reopen = Some(path);
        }
        Ok(Gone::Stayed(why)) => {
            view.goto = None;
            // The window does not scan the mounts the scan left out.
            front.state.status = Some(why.text(false));
        }
        Err(why) => {
            if let Some(g) = &mut view.goto {
                g.failed(why);
            }
        }
    }
}

/// `Tab` in the window with `text`: the completion or mounts at once, or asked for
/// ([`poll`]).
pub fn tab(front: &mut Front, text: &str) {
    let offer = session::tab(&mut front.state, text);
    let pending = front.state.goto.pending();
    if let Some(g) = &mut front.view.goto {
        match offer {
            Some(offer) => g.offered(offer),
            None => {
                g.edited();
                g.asking = pending;
            }
        }
        g.focus = true;
    }
}

/// The window's text changed to `text`, or it opened with it: the mounts are asked for when
/// nothing or only the root is typed.
pub fn edited(front: &mut Front, text: &str) {
    session::edited(&mut front.state, text);
    if let Some(g) = &mut front.view.goto {
        g.asking = front.state.goto.pending();
    }
}

/// Takes in what the computer answered for the window's text, once the session has it.
pub fn poll(front: &mut Front) {
    let Some(g) = &mut front.view.goto else {
        if front.state.goto.pending() {
            front.state.goto.clear();
        }
        return;
    };
    match session::take_answer(&mut front.state, &g.text) {
        Some(offer) => g.offered(offer),
        None if g.asking && !front.state.goto.pending() => g.asking = false,
        None => {}
    }
}

/// `text` with its last component, what is typed after the last separator of `host`, replaced
/// by `name` and a separator, so what is typed next goes below it.
pub fn with_name(text: &str, name: &str, host: Host) -> String {
    let cut = (text.bytes())
        .rposition(|b| host.is_sep(b))
        .map_or(0, |i| i + 1);
    format!("{}{name}{}", &text[..cut], char::from(host.sep()))
}

/// Draws the window while it is open: the path field, then what is wrong with it, the names a
/// `Tab` found, or what the keys do.
pub fn show(
    ctx: &egui::Context,
    s: &SessionState,
    colors: &Colors,
    view: &mut ViewState,
    out: &mut Vec<Intent>,
) {
    let Some(g) = &mut view.goto else { return };
    let mut close = false;
    let style = ctx.global_style();
    let frame = Frame::window(&style)
        .shadow(style.visuals.popup_shadow)
        .inner_margin(Margin::same(12));
    let modal_id = Id::unique("dfdu-goto-modal");
    let field_id = Id::unique(FIELD);
    let host = s.snap.dir_path.host();
    let modal = Modal::new(modal_id)
        .area(Modal::default_area(modal_id).anchor(Align2::CENTER_TOP, vec2(0.0, 72.0)))
        .backdrop_color(Color32::TRANSPARENT)
        .frame(frame)
        .show(ctx, |ui| {
            ui.set_width(WIDTH - 24.0);
            ui.label(kit::title("Go to folder", 13.0));
            ui.add_space(SPACE_1);
            // Tab completes; it does not move the keyboard to another widget.
            let focused = ui.memory(|m| m.has_focus(field_id));
            if focused && ui.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Tab)) {
                out.push(Intent::GoToTab(g.text.clone()));
            }
            let width = ui.available_width();
            let hint = "Type the path of a folder";
            let r = crate::windows::field(ui, colors, &mut g.text, field_id, hint, width);
            if r.has_focus() {
                let locked = EventFilter {
                    tab: true,
                    horizontal_arrows: true,
                    vertical_arrows: true,
                    escape: false,
                };
                ui.memory_mut(|m| m.set_focus_lock_filter(field_id, locked));
            }
            if std::mem::take(&mut g.focus) {
                r.request_focus();
                cursor_to_end(ui.ctx(), field_id, &g.text);
            }
            if r.changed() {
                g.edited();
                out.push(Intent::GoToEdited(g.text.clone()));
            }
            if r.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter)) {
                out.push(Intent::GoToFolder(g.text.clone()));
            }
            if r.lost_focus() && ui.input(|i| i.key_pressed(Key::Escape)) {
                close = true;
            }
            ui.add_space(SPACE_1);
            let dim = colors.fg(Token::FgDim);
            if let Some(e) = &g.error {
                ui.label(kit::sans(e.as_str(), 12.0).color(colors.fg(Token::StatusWarn)));
            } else if !g.mounts.is_empty() {
                let mut chosen = None;
                for m in g.mounts.iter().take(MOUNTS_SHOWN) {
                    ui.horizontal(|ui| {
                        let link = egui::Link::new(kit::mono(m.point.as_str(), 12.0).color(dim));
                        if ui.add(link).clicked() {
                            chosen = Some(m.clone());
                        }
                        ui.label(kit::sans(m.note(), 12.0).color(dim));
                    });
                }
                if g.mounts.len() > MOUNTS_SHOWN {
                    let more = g.mounts.len() - MOUNTS_SHOWN;
                    ui.label(kit::sans(format!("and {more} more"), 12.0).color(dim));
                }
                if let Some(m) = chosen {
                    g.choose_mount(&m, host);
                    out.push(Intent::GoToEdited(g.text.clone()));
                }
            } else if g.asking {
                ui.label(kit::sans("Asking the computer…", 12.0).color(dim));
            } else if g.candidates.is_empty() {
                let note = if s.from_file.is_none() {
                    "Tab completes · Enter goes there · a folder outside the scan is scanned"
                } else {
                    "Enter goes there"
                };
                ui.label(kit::sans(note, 12.0).color(dim));
            } else {
                let mut chosen = None;
                for name in g.candidates.iter().take(SHOWN) {
                    let link = egui::Link::new(kit::mono(name.as_str(), 12.0).color(dim));
                    if ui.add(link).clicked() {
                        chosen = Some(name.clone());
                    }
                }
                if g.candidates.len() > SHOWN {
                    let more = g.candidates.len() - SHOWN;
                    ui.label(kit::sans(format!("and {more} more"), 12.0).color(dim));
                }
                if let Some(name) = chosen {
                    g.choose(&name, host);
                    out.push(Intent::GoToEdited(g.text.clone()));
                }
            }
        });
    if close || modal.should_close() {
        view.goto = None;
    }
}

/// Puts the cursor of the field `id` at the end of `text`, which was set from outside it.
fn cursor_to_end(ctx: &egui::Context, id: Id, text: &str) {
    let mut state = egui::TextEdit::load_state(ctx, id).unwrap_or_default();
    let end = CCursor::new(text.chars().count());
    state.cursor.set_char_range(Some(CCursorRange::one(end)));
    state.store(ctx, id);
}

#[cfg(test)]
mod tests {
    use dfdu_session::goto::Kept;
    use dfdu_tree::HostPath;

    use super::*;

    #[test]
    fn a_candidate_replaces_what_is_typed_of_the_last_component() {
        assert_eq!(with_name("/mnt/di", "disk1", Host::Unix), "/mnt/disk1/");
        assert_eq!(with_name("/mnt/", "disk1", Host::Unix), "/mnt/disk1/");
        assert_eq!(with_name("di", "disk1", Host::Unix), "disk1/");
        assert_eq!(
            with_name("C:\\Users\\a", "alice", Host::Windows),
            "C:\\Users\\alice\\"
        );
    }

    #[test]
    fn a_tab_completes_lists_or_says_nothing_matches() {
        let mut g = GoTo {
            text: "/mnt/d".into(),
            ..GoTo::default()
        };
        g.completed(Completion {
            text: Some("/mnt/disk".into()),
            candidates: vec!["disk1".into(), "disk2".into()],
        });
        assert_eq!(g.text, "/mnt/disk");
        assert_eq!(g.candidates, ["disk1", "disk2"]);
        assert!(g.error.is_none() && g.focus);
        g.choose("disk2", Host::Unix);
        assert_eq!(g.text, "/mnt/disk2/");
        assert!(g.candidates.is_empty());
        g.completed(Completion::default());
        assert_eq!(g.text, "/mnt/disk2/");
        assert!(g.error.is_some());
    }

    /// A browser with the window open on `text`.
    fn front(text: &str) -> Front {
        let mut f = Front {
            state: SessionState::new(std::time::Instant::now()),
            view: ViewState::default(),
        };
        f.view.goto = Some(GoTo {
            text: text.into(),
            ..GoTo::default()
        });
        f
    }

    #[test]
    fn where_enter_went_closes_the_window_or_says_why_in_it() {
        let mut f = front("/r/lib");
        gone_to(&mut f, Ok(Gone::Within(None)));
        assert!(f.view.goto.is_none() && f.view.follow && f.view.reopen.is_none());

        let mut f = front("/elsewhere");
        let away = HostPath::from("/elsewhere");
        gone_to(&mut f, Ok(Gone::Outside(away.clone())));
        assert!(f.view.goto.is_none());
        assert_eq!(f.view.reopen, Some(away));

        let mut f = front("/r/disk");
        let kept = Kept::NotEntered(HostPath::from("/r/disk"));
        gone_to(&mut f, Ok(Gone::Stayed(kept.clone())));
        assert!(f.view.goto.is_none() && !f.view.follow);
        assert_eq!(f.state.status, Some(kept.text(false)), "no m in the window");

        let mut f = front("/x");
        gone_to(&mut f, Err("no such folder: /x".into()));
        let g = f.view.goto.as_ref().expect("still open");
        assert_eq!(g.error.as_deref(), Some("no such folder: /x"));
        assert!(g.focus);
    }

    #[test]
    fn what_the_computer_offers_replaces_what_was_shown() {
        let mut g = GoTo::default();
        let disk = Mount {
            point: "/mnt/disk1".into(),
            fstype: "xfs".into(),
            used: None,
            total: None,
        };
        g.offered(Offer::Mounts(vec![disk.clone()]));
        assert_eq!(g.mounts, std::slice::from_ref(&disk));
        g.choose_mount(&disk, Host::Unix);
        assert_eq!(g.text, "/mnt/disk1/");
        assert!(g.mounts.is_empty() && g.focus);
        g.asking = true;
        g.offered(Offer::Failed("permission denied".into()));
        assert!(!g.asking);
        assert_eq!(g.error.as_deref(), Some("permission denied"));
    }

    /// An empty field on a scan of this machine lists its mounts once they are read, off the
    /// frame; the answer goes into the window when the session has it.
    #[test]
    fn an_empty_field_lists_the_mounts_when_they_arrive() {
        let mut f = front("");
        f.state.outside.local = true;
        edited(&mut f, "");
        assert!(f.view.goto.as_ref().is_some_and(|g| g.asking));
        let until = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while !f.state.goto.poll() {
            assert!(
                std::time::Instant::now() < until,
                "the mounts were not read"
            );
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        poll(&mut f);
        let g = f.view.goto.as_ref().expect("open");
        assert!(!g.asking);
        assert!(g.error.is_none() || g.mounts.is_empty());

        // A window closed meanwhile drops the ask.
        edited(&mut f, "/");
        f.view.goto = None;
        poll(&mut f);
        assert!(!f.state.goto.pending());
    }

    #[test]
    fn a_tab_on_a_scan_read_from_a_file_finds_nothing() {
        let mut f = front("/r/a");
        f.state.snap.dir_path = HostPath::from("/r");
        tab(&mut f, "/r/a");
        let g = f.view.goto.as_ref().expect("open");
        assert!(!g.asking && g.error.is_some());
    }

    #[test]
    fn a_scan_read_from_a_file_points_to_the_start_screen() {
        let mut f = front("/elsewhere");
        f.state.from_file = Some(FROM_FILE);
        f.state.snap.root_path = HostPath::from("/r");
        f.state.snap.dir_path = HostPath::from("/r");
        let gone = session::go(&mut f.state, "/elsewhere", |_| unreachable!("outside"));
        gone_to(&mut f, gone);
        let g = f.view.goto.as_ref().expect("still open");
        assert_eq!(g.error.as_deref(), Some(FROM_FILE));
    }
}
