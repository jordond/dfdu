//! Go to folder (`Ctrl-G`, the command palette): a path typed in a prompt that starts with the
//! directory shown. `Tab` completes its last component from the directories on this machine
//! (a scan of another host shows none); `Enter` opens it, in the scan when it is inside it and
//! as a new scan otherwise; what is wrong with the path is shown below it and keeps the prompt
//! open. How paths resolve is in `dfdu_session::goto`.

use dfdu_session::goto::{self as session, Gone};
use ratatui::Frame;
use ratatui::crossterm::event::KeyEvent;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::{Line, Span};

use crate::app::{App, Effect, Overlay, views};
use crate::input::line::{Edit, LineInput};
use crate::input::line_keys;
use crate::view::input_spans;

/// Directory names shown below the prompt at most, after a `Tab` that found several.
const SHOWN: usize = 6;

/// State of the open prompt, held in `Overlay::GoTo`.
#[derive(Clone, Debug, Default)]
pub struct Prompt {
    input: LineInput,
    /// The names the last `Tab` found, when it found several.
    candidates: Vec<String>,
    /// What is wrong with the path the last `Enter` tried, or that `Tab` found nothing.
    error: Option<String>,
}

/// Opens the prompt with the directory shown.
pub fn open(app: &mut App) {
    let mut input = LineInput::default();
    input.set(&session::prefill(&app.session));
    app.overlay = Overlay::GoTo(Prompt {
        input,
        ..Prompt::default()
    });
}

/// Typing edits the path, `Tab` completes it, `Enter` goes there and `Esc` closes the prompt.
pub fn key(app: &mut App, k: KeyEvent) -> Vec<Effect> {
    let Overlay::GoTo(p) = &mut app.overlay else {
        return Vec::new();
    };
    if line_keys::completes(&k) {
        let c = session::complete_in(&app.session, p.input.text());
        let Overlay::GoTo(p) = &mut app.overlay else {
            return Vec::new();
        };
        if let Some(text) = &c.text {
            p.input.set(text);
        }
        p.error = (c.text.is_none() && c.candidates.is_empty())
            .then(|| "no folder here starts with that".to_owned());
        p.candidates = c.candidates;
        return Vec::new();
    }
    match line_keys::edit(&mut p.input, &k) {
        Edit::Changed => {
            p.candidates.clear();
            p.error = None;
        }
        Edit::Submit => {
            let text = p.input.text().to_owned();
            match session::go(&mut app.session, &text) {
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
                Err(e) => {
                    if let Overlay::GoTo(p) = &mut app.overlay {
                        p.error = Some(e);
                        p.candidates.clear();
                    }
                }
            }
        }
        Edit::Cancel => app.overlay = Overlay::None,
        Edit::Moved | Edit::Ignored => {}
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
    } else if p.candidates.is_empty() {
        let note = if app.session.outside.local {
            " Tab completes · Enter goes there · a folder outside the scan is scanned"
        } else {
            " Enter goes there · a folder outside the scan is scanned"
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
