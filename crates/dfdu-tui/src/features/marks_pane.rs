//! The marks pane: what is marked, in any directory, with each size and what deleting it all
//! would free, as dua's mark pane and DaisyDisk's Collector. `Tab` opens it from the details
//! pane, or from the list when the details are not shown, and it takes their place until `Tab`
//! or `Esc` gives the focus back to the list. It has its own cursor: `Space` unmarks the selected
//! item, `*` inverts the marks of the current directory, and `d` and `D` ask to delete what is
//! marked, as in the list.
//!
//! What it would free is the engine's estimate (`Command::Estimate`), asked for when the pane
//! opens and again after each key that changes the marks. Whether it is open, the cursor and
//! the estimate are the session's (`dfdu_session::marks::pane`); its scroll stays here.

use dfdu_session::marks::pane as session;
use ratatui::Frame;
use ratatui::crossterm::event::{KeyCode, KeyEvent};
use ratatui::layout::Rect;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Paragraph, Wrap};

use crate::app::keys::{self, Action, first_key};
use crate::app::{App, Effect};
use crate::format::{human, truncate_middle};
use crate::model::outside::rel_text;
use crate::state::marks;
use crate::view::columns::cursor;

/// Lines above the items: the count and size marked, what it would free, and a blank line.
const HEAD: usize = 3;
/// Cells of the size column.
const SIZE: usize = 10;

/// What only the terminal keeps of the pane: its scroll.
#[derive(Clone, Debug, Default)]
pub struct MarksPane {
    /// First item shown.
    scroll: usize,
}

/// True while the pane is open. It is open only with the focus.
pub fn is_open(app: &App) -> bool {
    app.session.marks_pane.open
}

/// Opens the pane with the focus, on the first item, and asks what the marks would free. The
/// caller takes the focus from the details pane.
pub fn show(app: &mut App) -> Vec<Effect> {
    app.marks_pane = MarksPane::default();
    session::open(&mut app.session)
}

/// Closes the pane, which gives the focus back to the list; false when it was not open.
pub fn close(app: &mut App) -> bool {
    session::close(&mut app.session)
}

/// Items one page moves.
fn page(app: &App) -> usize {
    app.areas.details.map_or(1, |r| {
        usize::from(r.height.saturating_sub(2))
            .saturating_sub(HEAD)
            .max(1)
    })
}

/// Moves the cursor by `delta` items. False when the pane is not shown, so the caller can move
/// the list instead.
pub fn scroll(app: &mut App, delta: i32) -> bool {
    if !is_open(app) {
        return false;
    }
    move_by(app, delta as isize);
    true
}

fn move_by(app: &mut App, delta: isize) {
    session::move_by(&mut app.session, delta);
}

/// Inverts the marks of the current directory, in this pane only: elsewhere `*` expands the
/// largest children in tree mode. The palette has the same command without a key.
pub const INVERT_KEY: char = '*';

/// Handles a key while the pane is open; `None` passes it on to the list. Keys are read
/// through [`keys::ACTIONS`], so the list's bindings move the cursor here, except
/// [`INVERT_KEY`], which only this pane has.
pub fn key(app: &mut App, k: &KeyEvent) -> Option<Vec<Effect>> {
    let page = page(app) as isize;
    let mut fx = Vec::new();
    let action = if k.code == KeyCode::Char(INVERT_KEY) {
        Action::InvertMarks
    } else {
        keys::action_for(k)?
    };
    match action {
        Action::Up => move_by(app, -1),
        Action::Down => move_by(app, 1),
        Action::PageUp => move_by(app, -page),
        Action::PageDown => move_by(app, page),
        Action::HalfPageUp => move_by(app, -(page / 2).max(1)),
        Action::HalfPageDown => move_by(app, (page / 2).max(1)),
        Action::First => app.session.marks_pane.cursor = 0,
        Action::Last => move_by(app, isize::MAX),
        Action::FocusDetails | Action::Close => {
            close(app);
            return Some(fx);
        }
        Action::ToggleMark => session::unmark(&mut app.session),
        Action::InvertMarks => {
            app.session.status = None;
            marks::invert(app);
        }
        a @ (Action::Trash | Action::DeletePermanent) => {
            app.session.status = None;
            if app.session.marks.is_empty() && !app.session.read_only {
                let space = first_key(Action::ToggleMark);
                app.session.status = Some(format!("nothing is marked: {space} in the list marks"));
            } else {
                fx = keys::run(app, a);
            }
        }
        _ => return None,
    }
    move_by(app, 0);
    fx.extend(session::ask(&mut app.session));
    Some(fx)
}

/// The path of a mark relative to the scan root, `/` after a directory.
fn label(app: &App, it: &marks::Item) -> String {
    let path = &it.target.path;
    let mut s = match rel_text(path, &app.session.snap.root_path) {
        Some(rel) if !rel.is_empty() => rel,
        _ => path.to_string(),
    };
    if it.is_dir {
        s.push('/');
    }
    s
}

/// What is marked and what deleting it would free, above the items.
fn head(app: &App) -> Vec<Line<'static>> {
    let t = &app.theme;
    let (total, scanning) = app.session.marks.total();
    let at_least = if scanning { "at least " } else { "" };
    let n = app.session.marks.items().len();
    let marked = Line::from(vec![
        Span::styled("● ", t.mark),
        Span::raw(format!("{n} marked, {at_least}{}", human(total))),
    ]);
    let frees = match session::estimate(&app.session) {
        Some(e) => {
            let stays = e.linked_out + e.shared_out;
            let mut s = format!(
                "would free {}{}",
                if e.scanning { "at least " } else { "about " },
                human(e.bytes)
            );
            if stays > 0 {
                s.push_str(&format!("; {} stays", human(stays)));
            }
            Line::styled(s, if stays > 0 { t.status_warn } else { t.fg_dim })
        }
        None => Line::styled("would free: estimating…", t.fg_dim),
    };
    vec![marked, frees, Line::raw("")]
}

/// What the pane says with nothing marked: how to mark.
fn empty(app: &App) -> Vec<Line<'static>> {
    let t = &app.theme;
    vec![
        Line::raw("Nothing is marked."),
        Line::raw(""),
        Line::styled(
            format!(
                "In the list, {} marks the selected entry and {} a range; here, {INVERT_KEY} inverts the marks of the current directory. Marks stay as you move between directories.",
                first_key(Action::ToggleMark),
                first_key(Action::RangeMark),
            ),
            t.fg_dim,
        ),
    ]
}

/// Draws the pane: a heavy border, as it always has the focus, the totals and one row per mark
/// with its size and path, the selected one in the cursor's colours.
pub fn draw(f: &mut Frame, app: &mut App, area: Rect) {
    let t = &app.theme;
    let mut block = Block::bordered()
        .border_type(BorderType::Thick)
        .border_style(t.border_focus)
        .title(Span::styled(" Marks ", t.title));
    let inner = block.inner(area);
    if app.session.marks.is_empty() {
        let text = empty(app);
        f.render_widget(
            Paragraph::new(text).wrap(Wrap { trim: true }).block(block),
            area,
        );
        return;
    }
    let fit = (inner.height as usize).saturating_sub(HEAD).max(1);
    move_by(app, 0);
    let at = app.session.marks_pane.cursor;
    let p = &mut app.marks_pane;
    p.scroll = p.scroll.min(at).max((at + 1).saturating_sub(fit));
    let app = &*app;
    let (t, p) = (&app.theme, &app.marks_pane);
    let width = inner.width;
    let name = (width as usize).saturating_sub(SIZE + 2);
    let mut lines = head(app);
    let items = app.session.marks.items();
    for (i, it) in items.iter().enumerate().skip(p.scroll).take(fit) {
        let path = truncate_middle(&label(app, it), name);
        let line = Line::from(format!("{:>SIZE$}  {path}", human(it.size)));
        // A mark inside another is deleted with it and not counted again.
        let line = if it.inside {
            line.style(t.fg_dim)
        } else {
            line
        };
        lines.push(if i == at {
            cursor(t, line, width)
        } else {
            line
        });
    }
    if p.scroll + fit < items.len() {
        block = block.title_bottom(Line::styled(" ↓ more ", t.fg_dim).right_aligned());
    }
    let shown = format!(" {}/{} ", at + 1, items.len());
    block = block.title_bottom(Line::styled(shown, t.fg_dim).left_aligned());
    f.render_widget(Paragraph::new(lines).block(block), area);
}

#[cfg(test)]
mod tests;
