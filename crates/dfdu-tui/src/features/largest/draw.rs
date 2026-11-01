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
use crate::app::keys::{Action, first_key};
use crate::features::review::{self, CHECK_WIDTH, Tally, Tick};
use crate::format::{bar, count, percent, ratio, truncate_middle};
use crate::model::outside::tilde;
use crate::view::cells;
use crate::view::columns::{self, Col, ColSet, blank, cursor, gaps, left, right};
use crate::view::empty::{self, Empty};

/// Rows above the files: the summary bar and the column names.
const HEAD: usize = 2;

pub fn draw(f: &mut Frame, app: &mut App, area: Rect) {
    let height = area.height as usize;
    let fit = height.saturating_sub(HEAD).max(1);
    app.session
        .set_viewport(dfdu_session::view::View::Largest, fit);
    let l = &mut app.session.largest;
    l.cursor = l.cursor.min(l.files.len().saturating_sub(1));
    let v = &mut app.largest;
    v.scroll = v
        .scroll
        .min(l.cursor)
        .max((l.cursor + 1).saturating_sub(fit));
    let scroll = v.scroll;
    let app = &*app;
    let (t, l) = (&app.theme, &app.session.largest);
    let dir = tilde(&app.session.snap.dir_path, app.session.home.as_deref());
    let width = area.width;
    if l.walked.is_none() {
        let text = format!(" reading the files below {dir}…");
        let text = truncate_middle(&text, usize::from(width));
        f.render_widget(Paragraph::new(Line::styled(text, t.fg_dim)), area);
        return;
    }
    if l.files.is_empty() {
        let e = Empty {
            title: "Largest",
            text: vec![
                "Lists the largest files anywhere below this directory, however deep.".into(),
                format!("No files below {dir}."),
            ],
            keys: vec![
                (first_key(Action::Parent), "parent directory"),
                (first_key(Action::Close), "back to Files"),
            ],
        };
        empty::draw(f, t, area, &e);
        return;
    }
    let mut tally = Tally::default();
    for file in &l.files {
        tally.add(file.size, app.session.marks.contains(file.node));
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
    let total = app.session.size_mode.of(&app.session.snap.dir_total).max(1);
    let shown = l.files.iter().skip(scroll).take(fit);
    let paths: Vec<String> = shown
        .clone()
        .map(|f| f.path.display().to_string())
        .collect();
    let shared = review::shared_prefix(paths.iter().map(String::as_str));
    for (i, (file, path)) in shown.zip(&paths).enumerate() {
        let line = row(app, file, path, shared, &cols, largest, total);
        lines.push(if scroll + i == l.cursor {
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
    for (gap, c, w) in gaps(cols) {
        spans.push(blank(gap));
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
    let tick = if app.session.marks.contains(file.node) {
        Tick::On
    } else {
        Tick::Off
    };
    let mut spans = vec![Span::raw(" "), review::checkbox(t, tick)];
    for (gap, c, w) in gaps(cols) {
        spans.push(blank(gap));
        match c {
            Col::Size => spans.push(right(crate::format::human(file.size), w, Style::new())),
            Col::Bar => {
                let frac = ratio(file.size, largest);
                spans.push(Span::styled(bar(frac, w.into(), t.bars), t.bar_fill));
            }
            Col::Share => {
                let share = format!("{:.1}%", percent(file.size, total));
                spans.push(right(share, w, Style::new()));
            }
            Col::Name => spans.extend(review::path(t, path, shared, w.into(), Style::new())),
            _ => spans.push(blank(w)),
        }
    }
    Line::from(spans)
}

#[cfg(test)]
mod tests {
    use std::time::Instant;

    use dfdu_tree::HostPath;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    use super::*;
    use crate::model::snapshot::test_entry;
    use crate::tests::{snap_with_totals, text};
    use crate::theme::Theme;

    const COLS: [(Col, u16); 4] = [
        (Col::Size, 10),
        (Col::Bar, 4),
        (Col::Share, 6),
        (Col::Name, 20),
    ];

    fn joined(spans: &[Span]) -> String {
        spans.iter().map(|s| s.content.as_ref()).collect()
    }

    /// A browser of `/r` holding `big.iso` and `x`, 4000 bytes each, and the file of `big.iso`
    /// at `a/big.iso`.
    fn browser() -> (App, File) {
        let t0 = Instant::now();
        let mut app = App::new(Theme::no_color(), t0);
        let entries = vec![test_entry(0, "big.iso", 4000), test_entry(1, "x", 4000)];
        app.refresh(snap_with_totals(entries, 1), t0);
        let file = File {
            node: app.session.snap.entries[0].node,
            path: HostPath::from("a/big.iso"),
            size: 4000,
            dev_ino: (1, 10),
            shared_out: 0,
        };
        (app, file)
    }

    #[test]
    fn the_header_names_the_path_column_after_the_checkbox() {
        let line = joined(&header(&COLS));
        let check = " ".repeat(CHECK_WIDTH + 1);
        assert_eq!(
            line,
            format!(
                "{check}{:>10}  {:4}  {:>6}  {:20}",
                "size", "", "share", "path"
            )
        );
    }

    #[test]
    fn a_row_has_its_tick_size_bar_share_and_path() {
        let (mut app, file) = browser();
        let row_of = |app: &App| row(app, &file, "a/big.iso", 2, &COLS, 8000, 16_000);
        let line = row_of(&app);
        let shown = joined(&line.spans);
        assert!(shown.starts_with(" [ ] "), "{shown}");
        assert!(shown.contains(&crate::format::human(4000)), "{shown}");
        assert!(shown.contains("25.0%"), "{shown}");
        // The part every path shares is dimmed, the rest is not.
        let t = &app.theme;
        assert!(
            line.spans
                .iter()
                .any(|s| s.content == "a/" && s.style == t.fg_dim)
        );
        assert!(line.spans.iter().any(|s| s.content == "big.iso"));
        app.session.move_to(0);
        dfdu_session::marks::toggle(&mut app.session);
        assert!(joined(&row_of(&app).spans).starts_with(" [x] "));
    }

    #[test]
    fn before_the_walk_it_says_what_it_reads() {
        let (mut app, _) = browser();
        app.session.largest.cursor = 5;
        let mut term = Terminal::new(TestBackend::new(60, 4)).expect("terminal");
        term.draw(|f| draw(f, &mut app, f.area())).expect("draw");
        let out = text(term.backend().buffer());
        assert!(out.starts_with(" reading the files below /r…"), "{out}");
        // The list has room for the rows below the summary bar and the column names.
        assert_eq!(app.session.viewport.rows, 2);
        assert_eq!(app.session.largest.cursor, 0, "kept within the files");
    }
}
