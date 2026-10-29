//! Drawing the view: a summary line, a dim line naming the columns, then one row per file in the
//! list's columns ([`columns::layout`]): the mark, size, a bar against the largest file, the
//! share of the directory, and the path relative to it, shortened in the middle.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use super::File;
use crate::app::App;
use crate::model::outside::tilde;
use crate::theme::{bar, count, human, percent, ratio, truncate_middle};
use crate::view::columns::{self, Col, ColSet, cursor, left, right};

/// Rows above the files: the summary and the column names.
const HEAD: usize = 2;

pub fn draw(f: &mut Frame, app: &mut App, area: Rect) {
    let height = area.height as usize;
    let fit = height.saturating_sub(HEAD).max(1);
    app.list_height = fit;
    let l = &mut app.largest;
    l.cursor = l.cursor.min(l.files.len().saturating_sub(1));
    l.scroll = l
        .scroll
        .min(l.cursor)
        .max((l.cursor + 1).saturating_sub(fit));
    let app = &*app;
    let (t, l) = (&app.theme, &app.largest);
    let dir = tilde(&app.snap.dir_path, app.home.as_deref());
    let summary = if l.walked.is_none() {
        format!(" reading the files below {dir}…")
    } else if l.files.is_empty() {
        format!(" no files below {dir}")
    } else {
        format!(
            " the largest {} of {} files below {dir} · {}/{}",
            l.files.len(),
            count(l.count),
            l.cursor + 1,
            l.files.len()
        )
    };
    let width = area.width;
    let summary = truncate_middle(&summary, usize::from(width));
    let mut lines = vec![Line::styled(summary, t.fg_dim)];
    if l.files.is_empty() {
        f.render_widget(Paragraph::new(lines), area);
        return;
    }
    let cols = columns::layout(width, ColSet::default());
    lines.push(header(&cols).style(t.fg_dim));
    let largest = l.files[0].size.max(1);
    let total = app.size_mode.of(&app.snap.dir_total).max(1);
    for (i, file) in l.files.iter().enumerate().skip(l.scroll).take(fit) {
        let line = row(app, file, &cols, largest, total);
        lines.push(if i == l.cursor {
            cursor(t, line, width)
        } else {
            line
        });
    }
    f.render_widget(Paragraph::new(lines), area);
}

/// The line naming the columns `cols`; the name column holds the path.
fn header(cols: &[(Col, u16)]) -> Line<'static> {
    let mut spans = Vec::new();
    let mut prev = None;
    for &(c, w) in cols {
        spans.push(Span::raw(" ".repeat(c.gap(prev).into())));
        prev = Some(c);
        spans.push(match c {
            Col::Name => left("path".into(), w, Style::new()),
            Col::Size | Col::Share => right(c.title().into(), w, Style::new()),
            _ => left(c.title().into(), w, Style::new()),
        });
    }
    Line::from(spans)
}

/// The row of `file`, with its bar against `largest` and its share of `total`.
fn row(app: &App, file: &File, cols: &[(Col, u16)], largest: u64, total: u64) -> Line<'static> {
    let t = &app.theme;
    let mut spans = Vec::new();
    let mut prev = None;
    for &(c, w) in cols {
        spans.push(Span::raw(" ".repeat(c.gap(prev).into())));
        prev = Some(c);
        match c {
            Col::Gutter => {
                let marked = app.marks.contains(file.node);
                let mark = if marked { "● " } else { "  " };
                spans.push(Span::styled(mark, t.mark));
            }
            Col::Size => spans.push(right(human(file.size), w, Style::new())),
            Col::Bar => {
                let frac = ratio(file.size, largest);
                spans.push(Span::styled(bar(frac, w.into(), t.bars), t.bar_fill));
            }
            Col::Share => {
                let share = format!("{:.1}%", percent(file.size, total));
                spans.push(right(share, w, Style::new()));
            }
            Col::Name => {
                let path = file.path.to_string();
                spans.push(Span::raw(truncate_middle(&path, w.into())));
            }
            _ => spans.push(Span::raw(" ".repeat(w.into()))),
        }
    }
    Line::from(spans)
}
