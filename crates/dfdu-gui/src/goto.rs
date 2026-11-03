//! Go to folder (Cmd-L or Cmd-Shift-G, Ctrl-L or Ctrl-Shift-G elsewhere, the registry's
//! `Ctrl-G` and the command palette): a path typed in a field that starts with the directory
//! shown. `Tab` completes its last component from the directories on this machine (a scan of
//! another computer shows none), and when several match they are listed below the field, each
//! one a link that completes to it. `Enter` goes there, in the scan when it is inside it and as
//! a new scan otherwise; what is wrong with the path is shown below the field, which keeps the
//! keyboard. `Esc` closes it. How paths resolve is in `dfdu_session::goto`.

use dfdu_session::SessionState;
use dfdu_session::goto::{self as session, Completion, Gone};
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
        self.candidates.clear();
        self.error = None;
        self.focus = true;
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
                g.completed(session::complete_in(s, &g.text));
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
                g.candidates.clear();
                g.error = None;
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
            } else if g.candidates.is_empty() {
                let note = if s.outside.local {
                    "Tab completes · Enter goes there · a folder outside the scan is scanned"
                } else {
                    "Enter goes there · a folder outside the scan is scanned"
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
