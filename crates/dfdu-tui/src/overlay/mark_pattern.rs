//! Mark by pattern (in the command palette): a glob over the names the list shows in the current
//! directory, such as `*.log` or `IMG_????.jpg`; `Enter` marks every entry it matches. Globs are
//! those of the rules ([`dfdu_rules::Glob`]): `*` and `?`, no `[...]`, and case ignored on macOS
//! and Windows, where file names usually ignore it.

use dfdu_rules::{Glob, GlobError, Platform};
use ratatui::Frame;
use ratatui::crossterm::event::KeyEvent;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::{Line, Span};

use crate::app::keys::READ_ONLY;
use crate::app::{App, Effect, Overlay};
use crate::input::line::{Edit, LineInput};
use crate::state::marks;
use crate::theme::human;

/// State of the open prompt, held in `Overlay::MarkPattern`.
#[derive(Clone, Debug, Default)]
pub struct Prompt {
    input: LineInput,
    /// How many names the glob typed matches, or what is wrong with it; counted as it is typed,
    /// not with every frame.
    matches: Option<Result<usize, GlobError>>,
}

/// The glob of `pattern`, compared as file names are on this platform.
fn glob(pattern: &str) -> Result<Glob, GlobError> {
    let fold = matches!(Platform::current(), Platform::MacOs | Platform::Windows);
    Glob::new(pattern.trim(), fold)
}

/// The entries the list shows that `g` matches, by index into the snapshot's entries. The
/// "shared between entries" row is not an entry, and directories are left out while a type
/// filter shows only some of their files.
fn matched(app: &App, g: &Glob) -> Vec<usize> {
    let folders = !crate::overlay::type_filter::active(app);
    (app.rows.iter().copied())
        .filter(|&i| {
            let e = &app.snap.entries[i];
            !e.pseudo && (folders || !e.is_dir) && g.matches(&e.name)
        })
        .collect()
}

/// Opens the prompt, empty. A read-only browser marks nothing, so it says so instead.
pub fn open(app: &mut App) {
    if app.read_only {
        app.status = Some(READ_ONLY.into());
        return;
    }
    app.overlay = Overlay::MarkPattern(Prompt::default());
}

/// Typing edits the glob; `Enter` marks what it matches and closes the prompt, `Esc` closes it.
pub fn key(app: &mut App, k: &KeyEvent) -> Vec<Effect> {
    let Overlay::MarkPattern(p) = &mut app.overlay else {
        return Vec::new();
    };
    match p.input.key(k) {
        Edit::Changed => {
            let text = p.input.text().trim().to_owned();
            let matches = (!text.is_empty()).then(|| glob(&text).map(|g| matched(app, &g).len()));
            if let Overlay::MarkPattern(p) = &mut app.overlay {
                p.matches = matches;
            }
        }
        Edit::Submit => {
            let text = p.input.text().trim().to_owned();
            app.overlay = Overlay::None;
            mark(app, &text);
        }
        Edit::Cancel => app.overlay = Overlay::None,
        Edit::Moved | Edit::Ignored => {}
    }
    Vec::new()
}

/// Marks the entries `pattern` matches, and says how many on the status line.
fn mark(app: &mut App, pattern: &str) {
    let g = match glob(pattern) {
        Ok(g) => g,
        Err(e) => {
            app.status = Some(format!("mark by pattern: {e}"));
            return;
        }
    };
    let hits = matched(app, &g);
    if hits.is_empty() {
        app.status = Some(format!("nothing here matches {pattern}"));
        return;
    }
    let items: Vec<marks::Item> = (hits.iter()).map(|&i| marks::item_of(app, i)).collect();
    let size: u64 = items.iter().map(|it| it.size).sum();
    let n = items.len();
    let added = marks::add(app, items);
    app.status = Some(match added {
        0 => format!("everything matching {pattern} is marked already"),
        _ => format!(
            "marked {added} of {n} entr{} matching {pattern} ({})",
            if n == 1 { "y" } else { "ies" },
            human(size)
        ),
    });
}

/// Draws the prompt centred in `area` and returns the rectangle it covers: the glob, and below
/// it how many names it matches or what is wrong with it.
pub fn draw(f: &mut Frame, app: &App, area: Rect) -> Rect {
    let Overlay::MarkPattern(p) = &app.overlay else {
        return Rect::default();
    };
    let t = &app.theme;
    let mut prompt = vec![Span::styled(" glob", t.key), Span::raw(" ")];
    prompt.extend(p.input.spans(Style::new(), t.key));
    let note = match &p.matches {
        None => Line::styled(" such as *.log or IMG_????.jpg", t.fg_dim),
        Some(Ok(n)) => Line::styled(
            format!(
                " {n} of {} names here match · Enter marks them",
                app.rows.len()
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
