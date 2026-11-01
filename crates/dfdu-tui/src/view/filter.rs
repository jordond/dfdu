//! The filter drawn ([`crate::state::filter`]): its line above the status line, and the names
//! of the list with the characters the query matches highlighted.

use ratatui::style::Style;
use ratatui::text::{Line, Span};

use super::input_spans;
use crate::app::App;
use crate::input::fuzzy::char_pos;
use crate::state::filter;

/// The filter's line above the status line, while it has one: the input while editing, else
/// the query in force.
pub fn line(app: &App) -> Option<Line<'static>> {
    let (f, t) = (&app.session.filter, &app.theme);
    let editing = filter::editing(app);
    if !editing && !f.active() {
        return None;
    }
    let (shown, of) = f.shown(app.session.hide_hidden);
    let spans = if editing {
        let mut spans = vec![Span::styled(" /", t.key), Span::raw(" ")];
        spans.extend(input_spans(f.input(), Style::new(), t.key));
        spans.push(Span::styled(format!("   {shown} of {of}"), t.fg_dim));
        spans
    } else {
        vec![
            Span::styled(" filter: ", t.fg_dim),
            Span::raw(format!("\"{}\"", f.input().text())),
            Span::styled(format!(" ({shown} of {of})   "), t.fg_dim),
            Span::styled("/", t.key),
            Span::styled(" edit  ", t.fg_dim),
            Span::styled("Esc", t.key),
            Span::styled(" clear", t.fg_dim),
        ]
    };
    Some(Line::from(spans))
}

/// `name` drawn in `style`, with the characters the query matches highlighted.
pub fn name_spans(app: &App, name: String, style: Style) -> Vec<Span<'static>> {
    let Some(indices) = app.session.filter.indices(&name) else {
        return vec![Span::styled(name, style)];
    };
    let matched = style.patch(app.theme.r#match);
    let mut spans = Vec::new();
    let mut run = String::new();
    let mut run_hit = false;
    let mut next = indices.iter().copied().peekable();
    for (i, ch) in name.chars().enumerate() {
        let hit = next.next_if_eq(&char_pos(i)).is_some();
        if hit != run_hit && !run.is_empty() {
            let s = if run_hit { matched } else { style };
            spans.push(Span::styled(std::mem::take(&mut run), s));
        }
        run_hit = hit;
        run.push(ch);
    }
    if !run.is_empty() {
        spans.push(Span::styled(run, if run_hit { matched } else { style }));
    }
    spans
}
