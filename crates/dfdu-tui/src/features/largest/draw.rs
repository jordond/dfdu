//! Drawing the view: the summary bar ([`review::summary`]), a dim line naming the columns with
//! how many files the walk found, then one row per file: its checkbox, then the list's columns
//! ([`columns::layout`]): size, a bar against the largest file, the share of the directory, and
//! the path relative to it, shortened in the middle with the part all files share dimmed.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use super::File;
use crate::app::App;
use crate::features::review::{self, CHECK_WIDTH, Tally, Tick};
use crate::model::outside::tilde;
use crate::theme::{bar, count, percent, ratio, truncate_middle};
use crate::view::cells;
use crate::view::columns::{self, Col, ColSet, cursor, left, right};

/// Rows above the files: the summary bar and the column names.
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
    let width = area.width;
    if l.walked.is_none() || l.files.is_empty() {
        let text = if l.walked.is_none() {
            format!(" reading the files below {dir}…")
        } else {
            format!(" no files below {dir}")
        };
        let text = truncate_middle(&text, usize::from(width));
        f.render_widget(Paragraph::new(Line::styled(text, t.fg_dim)), area);
        return;
    }
    let mut tally = Tally::default();
    for file in &l.files {
        tally.add(file.size, app.marks.contains(file.node));
    }
    let found = format!(
        " · the largest {} of {} files below {dir}",
        l.files.len(),
        count(l.count)
    );
    let mut lines = vec![review::summary(app, tally, "files", &found, width)];
    // The layout's gutter gives way to the checkbox.
    let row_width = width.saturating_sub(cells(CHECK_WIDTH + 1));
    let gutter = columns::layout(width, ColSet::default())
        .iter()
        .find(|(c, _)| *c == Col::Gutter)
        .map_or(0, |&(_, w)| w);
    let cols: Vec<(Col, u16)> = columns::layout(row_width + gutter, ColSet::default())
        .into_iter()
        .filter(|(c, _)| *c != Col::Gutter)
        .collect();
    lines.push(Line::from(header(&cols)).style(t.fg_dim));
    let largest = l.files[0].size.max(1);
    let total = app.size_mode.of(&app.snap.dir_total).max(1);
    let shown = l.files.iter().skip(l.scroll).take(fit);
    let paths: Vec<String> = shown
        .clone()
        .map(|f| f.path.display().to_string())
        .collect();
    let shared = review::shared_prefix(paths.iter().map(String::as_str));
    for (i, (file, path)) in shown.zip(&paths).enumerate() {
        let line = row(app, file, path, shared, &cols, largest, total);
        lines.push(if l.scroll + i == l.cursor {
            cursor(t, line, width)
        } else {
            line
        });
    }
    f.render_widget(Paragraph::new(lines), area);
}

/// The line naming the columns `cols`, after the checkbox's; the name column holds the path.
fn header(cols: &[(Col, u16)]) -> Vec<Span<'static>> {
    let mut spans = vec![Span::raw(" ".repeat(CHECK_WIDTH + 1))];
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
    spans
}

/// The row of `file` at `path`, its first `shared` characters dimmed, with its bar against
/// `largest` and its share of `total`.
fn row(
    app: &App,
    file: &File,
    path: &str,
    shared: usize,
    cols: &[(Col, u16)],
    largest: u64,
    total: u64,
) -> Line<'static> {
    let t = &app.theme;
    let tick = if app.marks.contains(file.node) {
        Tick::On
    } else {
        Tick::Off
    };
    let mut spans = vec![Span::raw(" "), review::checkbox(t, tick)];
    let mut prev = None;
    for &(c, w) in cols {
        spans.push(Span::raw(" ".repeat(c.gap(prev).into())));
        prev = Some(c);
        match c {
            Col::Size => spans.push(right(crate::theme::human(file.size), w, Style::new())),
            Col::Bar => {
                let frac = ratio(file.size, largest);
                spans.push(Span::styled(bar(frac, w.into(), t.bars), t.bar_fill));
            }
            Col::Share => {
                let share = format!("{:.1}%", percent(file.size, total));
                spans.push(right(share, w, Style::new()));
            }
            Col::Name => spans.extend(review::path(t, path, shared, w.into(), Style::new())),
            _ => spans.push(Span::raw(" ".repeat(w.into()))),
        }
    }
    Line::from(spans)
}
