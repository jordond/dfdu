//! Go to folder (`Ctrl-G`, the command palette): a path typed in a prompt that starts with the
//! directory shown. `Tab` completes its last component from the directories on the scanned
//! host (another host's are asked of its agent, and shown when they arrive, [`poll`]). With
//! nothing or only the root typed the host's mounts are listed instead; `↑`/`↓` pick one and
//! `Tab` or `Enter` puts it in the prompt. `Enter` opens the path, in the scan when it is
//! inside it and as a new scan otherwise; what is wrong with the path is shown below it and
//! keeps the prompt open. A path inside the scan where the tree has no folder closes the prompt and says so on
//! the status line. The run loop goes to the path submitted with the session's tree
//! ([`submitted`], [`gone_to`]). How paths resolve is in `dfdu_session::goto`.

use dfdu_session::goto::{self as session, Completion, Gone, Mount, Offer};
use ratatui::Frame;
use ratatui::crossterm::event::{KeyCode, KeyEvent};
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::{Line, Span};

use crate::app::{App, Effect, Overlay, views};
use crate::input::line::{Edit, LineInput};
use crate::input::line_keys;
use crate::view::input_spans;

/// Directory names shown below the prompt at most, after a `Tab` that found several.
const SHOWN: usize = 6;
/// Mounts listed at most.
const MOUNTS_SHOWN: usize = 10;

/// State of the open prompt, held in `Overlay::GoTo`.
#[derive(Clone, Debug, Default)]
pub struct Prompt {
    input: LineInput,
    /// The names the last `Tab` found, when it found several.
    candidates: Vec<String>,
    /// The host's mounts, while nothing or only the root is typed.
    mounts: Vec<Mount>,
    /// The mount `↑`/`↓` picked.
    pick: Option<usize>,
    /// The host was asked and has not answered yet.
    asking: bool,
    /// What is wrong with the path the last `Enter` tried, or that `Tab` found nothing.
    error: Option<String>,
    /// `Enter` was pressed: the run loop goes to the path ([`submitted`]).
    submitted: bool,
}

impl Prompt {
    /// Takes in what the host offers for the text shown.
    fn offered(&mut self, offer: Offer) {
        self.asking = false;
        self.candidates.clear();
        self.mounts.clear();
        self.pick = None;
        self.error = None;
        match offer {
            Offer::Completed(c) => self.completed(c),
            Offer::Mounts(m) => self.mounts = m,
            Offer::Failed(why) => self.error = Some(why),
        }
    }

    fn completed(&mut self, c: Completion) {
        if let Some(text) = &c.text {
            self.input.set(text);
        }
        self.error = (c.text.is_none() && c.candidates.is_empty())
            .then(|| "no folder here starts with that".to_owned());
        self.candidates = c.candidates;
    }

    /// Clears what was shown for the text before an edit.
    fn edited(&mut self) {
        self.candidates.clear();
        self.mounts.clear();
        self.pick = None;
        self.asking = false;
        self.error = None;
    }
}

/// Opens the prompt with the directory shown; at the root of the filesystem the host's mounts
/// are asked for.
pub fn open(app: &mut App) {
    let mut input = LineInput::default();
    input.set(&session::prefill(&app.session));
    session::edited(&mut app.session, input.text());
    app.overlay = Overlay::GoTo(Prompt {
        input,
        asking: app.session.goto.pending(),
        ..Prompt::default()
    });
}

/// Takes in what the host answered for the prompt's text, once the session has it: true when
/// the prompt changed.
pub fn poll(app: &mut App) -> bool {
    let Overlay::GoTo(p) = &mut app.overlay else {
        return false;
    };
    let Some(offer) = session::take_answer(&mut app.session, p.input.text()) else {
        if p.asking && !app.session.goto.pending() {
            // Asked for another text, or the prompt's ask was dropped.
            p.asking = false;
            return true;
        }
        return false;
    };
    p.offered(offer);
    true
}

/// Typing edits the path, `Tab` completes it, `Enter` goes there and `Esc` closes the prompt.
pub fn key(app: &mut App, k: KeyEvent) -> Vec<Effect> {
    let Overlay::GoTo(p) = &mut app.overlay else {
        return Vec::new();
    };
    if !p.mounts.is_empty() && matches!(k.code, KeyCode::Up | KeyCode::Down) {
        let last = p.mounts.len().min(MOUNTS_SHOWN) - 1;
        p.pick = Some(match (p.pick, k.code) {
            (None, KeyCode::Up) => last,
            (None, _) => 0,
            (Some(i), KeyCode::Up) => i.saturating_sub(1),
            (Some(i), _) => (i + 1).min(last),
        });
        return Vec::new();
    }
    let host = app.session.snap.dir_path.host();
    let chosen = (p.pick.and_then(|i| p.mounts.get(i))).map(|m| m.text(host));
    if let Some(text) = chosen
        && (line_keys::completes(&k) || k.code == KeyCode::Enter)
    {
        p.input.set(&text);
        p.edited();
        session::edited(&mut app.session, &text);
        p.asking = app.session.goto.pending();
        return Vec::new();
    }
    if line_keys::completes(&k) {
        let text = p.input.text().to_owned();
        let offer = session::tab(&mut app.session, &text);
        let Overlay::GoTo(p) = &mut app.overlay else {
            return Vec::new();
        };
        match offer {
            Some(offer) => p.offered(offer),
            None => {
                p.edited();
                p.asking = true;
            }
        }
        return Vec::new();
    }
    match line_keys::edit(&mut p.input, &k) {
        Edit::Changed => {
            p.edited();
            let text = p.input.text().to_owned();
            session::edited(&mut app.session, &text);
            if let Overlay::GoTo(p) = &mut app.overlay {
                p.asking = app.session.goto.pending();
            }
        }
        Edit::Submit => p.submitted = true,
        Edit::Cancel => {
            app.session.goto.clear();
            app.overlay = Overlay::None;
        }
        Edit::Moved | Edit::Ignored => {}
    }
    Vec::new()
}

/// The path `Enter` submitted, once, for the run loop to go to
/// ([`dfdu_session::Session::go_to_path`]).
pub fn submitted(app: &mut App) -> Option<String> {
    let Overlay::GoTo(p) = &mut app.overlay else {
        return None;
    };
    std::mem::take(&mut p.submitted).then(|| p.input.text().to_owned())
}

/// Takes in where the path submitted went. The prompt closes on a folder of the scan, which
/// the list opens; on a path outside it, which the run loop scans; and on a path inside it
/// where the tree has no folder, which the status line explains. What is wrong with the path
/// keeps it open.
pub fn gone_to(app: &mut App, gone: Result<Gone, String>) -> Vec<Effect> {
    match gone {
        Ok(Gone::Within(moved)) => {
            app.overlay = Overlay::None;
            let then = dfdu_session::view::Then {
                entered: moved,
                ..Default::default()
            };
            return views::carry(app, then);
        }
        Ok(Gone::Outside(path)) => {
            app.overlay = Overlay::None;
            app.reopen = Some(path);
        }
        Ok(Gone::Stayed(why)) => {
            app.overlay = Overlay::None;
            app.session.status = Some(why.text(true));
        }
        Err(e) => {
            if let Overlay::GoTo(p) = &mut app.overlay {
                p.error = Some(e);
                p.candidates.clear();
            }
        }
    }
    Vec::new()
}

/// Draws the prompt centred in `area` and returns the rectangle it covers: the path, and below
/// it what is wrong with it, the names a `Tab` found, or what the keys do.
pub fn draw(f: &mut Frame, app: &App, area: Rect) -> Rect {
    let Overlay::GoTo(p) = &app.overlay else {
        return Rect::default();
    };
    let t = &app.theme;
    let mut prompt = vec![Span::raw(" ")];
    prompt.extend(input_spans(&p.input, Style::new(), t.key));
    let mut lines = vec![Line::from(prompt)];
    if let Some(e) = &p.error {
        lines.push(Line::styled(format!(" {e}"), t.status_warn));
    } else if !p.mounts.is_empty() {
        let width = (p.mounts.iter().take(MOUNTS_SHOWN))
            .map(|m| m.point.chars().count())
            .max()
            .unwrap_or(0);
        for (i, m) in p.mounts.iter().take(MOUNTS_SHOWN).enumerate() {
            let picked = p.pick == Some(i);
            let mark = if picked { "›" } else { " " };
            let style = if picked { t.key } else { t.fg_dim };
            lines.push(Line::from(vec![
                Span::styled(format!("{mark}{:<width$}  ", m.point), style),
                Span::styled(m.note(), t.fg_dim),
            ]));
        }
        if p.mounts.len() > MOUNTS_SHOWN {
            let more = p.mounts.len() - MOUNTS_SHOWN;
            lines.push(Line::styled(format!(" and {more} more"), t.fg_dim));
        }
        lines.push(Line::styled(
            " ↑↓ pick a mount · Tab puts it here",
            t.fg_dim,
        ));
    } else if p.asking {
        lines.push(Line::styled(" asking the host…", t.fg_dim));
    } else if p.candidates.is_empty() {
        let note = if app.session.from_file.is_none() {
            " Tab completes · Enter goes there · a folder outside the scan is scanned"
        } else {
            " Enter goes there"
        };
        lines.push(Line::styled(note, t.fg_dim));
    } else {
        for name in p.candidates.iter().take(SHOWN) {
            lines.push(Line::styled(format!(" {name}"), t.fg_dim));
        }
        if p.candidates.len() > SHOWN {
            let more = p.candidates.len() - SHOWN;
            lines.push(Line::styled(format!(" and {more} more"), t.fg_dim));
        }
    }
    let width = super::width(area, 76);
    super::dialog(f, area, width, " Go to folder ", lines)
}

#[cfg(test)]
mod tests;
