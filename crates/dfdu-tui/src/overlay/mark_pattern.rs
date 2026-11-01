//! Mark by pattern (in the command palette): a glob over the names the list shows in the current
//! directory, such as `*.log` or `IMG_????.jpg`; `Enter` marks every entry it matches. Globs are
//! those of the rules ([`dfdu_rules::Glob`]): `*` and `?`, no `[...]`, and case ignored on macOS
//! and Windows, where file names usually ignore it. The glob and what it matches are in
//! `dfdu_session::mark_pattern`.

use dfdu_rules::GlobError;
use dfdu_session::mark_pattern as session;
use ratatui::Frame;
use ratatui::crossterm::event::KeyEvent;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::{Line, Span};

use crate::app::keys::READ_ONLY;
use crate::app::{App, Effect, Overlay};
use crate::input::line::{Edit, LineInput};
use crate::input::line_keys;
use crate::view::input_spans;

/// State of the open prompt, held in `Overlay::MarkPattern`.
#[derive(Clone, Debug, Default)]
pub struct Prompt {
    input: LineInput,
    /// How many names the glob typed matches, or what is wrong with it; counted as it is typed,
    /// not with every frame.
    matches: Option<Result<usize, GlobError>>,
}

/// Opens the prompt, empty. A read-only browser marks nothing, so it says so instead.
pub fn open(app: &mut App) {
    if app.session.read_only {
        app.session.status = Some(READ_ONLY.into());
        return;
    }
    app.overlay = Overlay::MarkPattern(Prompt::default());
}

/// Typing edits the glob; `Enter` marks what it matches and closes the prompt, `Esc` closes it.
pub fn key(app: &mut App, k: KeyEvent) -> Vec<Effect> {
    let Overlay::MarkPattern(p) = &mut app.overlay else {
        return Vec::new();
    };
    match line_keys::edit(&mut p.input, &k) {
        Edit::Changed => {
            let matches = session::count(&app.session, p.input.text());
            if let Overlay::MarkPattern(p) = &mut app.overlay {
                p.matches = matches;
            }
        }
        Edit::Submit => {
            let text = p.input.text().trim().to_owned();
            app.overlay = Overlay::None;
            session::mark(&mut app.session, &text);
        }
        Edit::Cancel => app.overlay = Overlay::None,
        Edit::Moved | Edit::Ignored => {}
    }
    Vec::new()
}

/// Draws the prompt centred in `area` and returns the rectangle it covers: the glob, and below
/// it how many names it matches or what is wrong with it.
pub fn draw(f: &mut Frame, app: &App, area: Rect) -> Rect {
    let Overlay::MarkPattern(p) = &app.overlay else {
        return Rect::default();
    };
    let t = &app.theme;
    let mut prompt = vec![Span::styled(" glob", t.key), Span::raw(" ")];
    prompt.extend(input_spans(&p.input, Style::new(), t.key));
    let note = match &p.matches {
        None => Line::styled(" such as *.log or IMG_????.jpg", t.fg_dim),
        Some(Ok(n)) => Line::styled(
            format!(
                " {n} of {} names here match · Enter marks them",
                app.session.list.rows.len()
            ),
            t.fg_dim,
        ),
        Some(Err(e)) => Line::styled(format!(" {e}"), t.status_warn),
    };
    let width = super::width(area, 60);
    super::dialog(
        f,
        area,
        width,
        " Mark by pattern ",
        vec![Line::from(prompt), note],
    )
}

#[cfg(test)]
mod tests;
