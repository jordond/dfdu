//! The entry list: a dim header row naming the columns, then one row per entry with the columns
//! [`columns::layout`] picks for the width (size, bar, share, items, change, `↔`, category) and
//! the name with its notes, and a scrollbar when the rows do not fit.

pub(super) mod name;

use std::time::Instant;

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Paragraph, Scrollbar, ScrollbarOrientation, ScrollbarState};

use super::columns::{self, CATEGORY_MAX, Col, ColSet};
use crate::app::App;
use crate::model::growth::{self, RowNote};
use crate::model::snapshot::{EntryData, Mark};
use crate::state::{marks, motion};
use crate::theme::{Theme, bar, count, human, percent, ratio, spinner};

/// Draws the column header on `head` and the rows on `area`.
pub fn draw(f: &mut Frame, app: &mut App, head: Rect, area: Rect, now: Instant, frame: u64) {
    let height = area.height as usize;
    app.list_height = height;
    let extra = usize::from(app.new_count > 0);
    let rows = app.rows.len();
    // Never scroll further than needed to fill the screen, then keep the selection visible.
    app.scroll = app.scroll.min((rows + extra).saturating_sub(height));
    if let Some(sel) = app.selected_index() {
        if sel < app.scroll {
            app.scroll = sel;
        } else if sel >= app.scroll + height.saturating_sub(extra).max(1) {
            app.scroll = sel + 1 - height.saturating_sub(extra).max(1);
        }
    }
    app.scroll = app.scroll.min(rows.saturating_sub(1));
    // Entry indices of the rows on screen.
    let visible: Vec<usize> = app
        .rows
        .iter()
        .skip(app.scroll)
        .take(height.saturating_sub(extra))
        .copied()
        .collect();
    motion::frame(app, now, &visible);

    let app = &*app;
    let t = &app.theme;
    let changes: Vec<Option<RowNote>> = visible.iter().map(|&i| change(app, i)).collect();
    let scrollbar = rows + extra > height;
    let mut cols = columns::layout(area.width, present(app, &visible, &changes));
    if scrollbar && let Some(name) = cols.last_mut() {
        // The name gives up the scrollbar's cell.
        name.1 = name.1.saturating_sub(1);
    }
    let width = area.width.saturating_sub(u16::from(scrollbar));
    f.render_widget(Paragraph::new(header(app, &cols)), head);

    let mut lines = Vec::with_capacity(height);
    if rows == 0 && app.new_count == 0 {
        let msg = match app.snap.dir_mark {
            Some(Mark::Scanning | Mark::Rescanning | Mark::Stalled) => "  (scanning…)",
            Some(Mark::Error) => "  (could not be read)",
            Some(Mark::Incomplete) => "  (incomplete: press r to rescan)",
            _ => "  (empty)",
        };
        lines.push(Line::styled(msg, t.fg_dim));
    }
    let max = app.rows_largest.max(1);
    let total = app.size_mode.of(&app.snap.dir_total).max(1);
    let sel = app.selected_index();
    for (row, (&i, &change)) in visible.iter().zip(&changes).enumerate() {
        let row_of = Row {
            index: i,
            frame,
            max,
            total,
            change,
        };
        let mut line = entry_line(app, &row_of, &cols);
        if sel == Some(app.scroll + row) {
            line = cursor(t, line, width);
        }
        lines.push(line);
    }
    if app.new_count > 0 && lines.len() < height {
        let n = app.new_count;
        lines.push(Line::styled(
            format!(
                "   + {n} new entr{} since last sort (largest {})   S to sort now",
                if n == 1 { "y" } else { "ies" },
                human(app.new_largest)
            ),
            t.fg_dim,
        ));
    }
    f.render_widget(Paragraph::new(lines), area);
    if scrollbar {
        let mut state = ScrollbarState::new((rows + extra).saturating_sub(height) + 1)
            .position(app.scroll)
            .viewport_content_length(height);
        let bar = Scrollbar::new(ScrollbarOrientation::VerticalRight)
            .begin_symbol(None)
            .end_symbol(None)
            .track_symbol(Some("│"))
            .track_style(t.border)
            .thumb_symbol("┃")
            .thumb_style(t.fg_dim);
        f.render_stateful_widget(bar, area, &mut state);
    }
}

/// What entry `index` changed since the last recorded scan, when that is worth a note.
fn change(app: &App, index: usize) -> Option<RowNote> {
    let e = &app.snap.entries[index];
    if app.growth.is_none() || e.ghost || e.pseudo {
        return None;
    }
    let path = app.snap.dir_path.join(dfdu_tree::name_os(&e.name));
    growth::row_note(app, &path, e.size, e.mark)
}

/// The optional columns the rows on screen (`visible`, with their `changes`) have something
/// to show in, of those the user asked for.
fn present(app: &App, visible: &[usize], changes: &[Option<RowNote>]) -> ColSet {
    let entries = || visible.iter().map(|&i| &app.snap.entries[i]);
    let category = app.snap.rules.as_ref().map_or(0, |rules| {
        entries()
            .filter_map(|e| e.tag)
            .map(|g| Span::raw(g.category(rules).0).width())
            .max()
            .unwrap_or(0)
    });
    ColSet {
        items: app.columns.items(),
        mtime: app.columns.mtime(),
        delta: app.columns.delta() && changes.iter().any(Option::is_some),
        shared: entries().any(|e| e.shared_out > 0),
        category: u16::try_from(category)
            .unwrap_or(CATEGORY_MAX)
            .min(CATEGORY_MAX),
    }
}

/// The dim line above the rows that names the columns.
fn header(app: &App, cols: &[(Col, u16)]) -> Line<'static> {
    let mut spans = Vec::new();
    let mut prev = None;
    for &(c, w) in cols {
        spans.push(Span::raw(" ".repeat(c.gap(prev).into())));
        prev = Some(c);
        let title = match c {
            Col::Delta => growth::column_title(app),
            c => c.title().to_owned(),
        };
        let title = if Span::raw(&title).width() > usize::from(w) {
            String::new()
        } else {
            title
        };
        spans.push(match c {
            Col::Size | Col::Share | Col::Items | Col::Delta => right(title, w, Style::new()),
            _ => left(title, w, Style::new()),
        });
    }
    Line::from(spans).style(app.theme.fg_dim)
}

/// `s` right-aligned in `w` cells.
fn right(s: String, w: u16, style: Style) -> Span<'static> {
    let pad = usize::from(w).saturating_sub(Span::raw(&s).width());
    Span::styled(format!("{}{s}", " ".repeat(pad)), style)
}

/// `s` left-aligned in `w` cells, cut to fit.
fn left(s: String, w: u16, style: Style) -> Span<'static> {
    let s = name::cut_end(&s, w.into());
    let pad = usize::from(w).saturating_sub(Span::raw(&s).width());
    Span::styled(format!("{s}{}", " ".repeat(pad)), style)
}

/// The cursor row: `line` filled to `width` cells in the cursor style, with dim text in the
/// normal text colour so it stays readable on the cursor's background.
fn cursor(t: &Theme, line: Line<'static>, width: u16) -> Line<'static> {
    let dim = t.fg_dim;
    let mut spans = line.spans;
    for s in &mut spans {
        let is_dim = (dim.fg.is_some() && s.style.fg == dim.fg)
            || (!dim.add_modifier.is_empty() && s.style.add_modifier.contains(dim.add_modifier));
        if is_dim {
            s.style.fg = t.fg.fg;
            s.style.add_modifier = s.style.add_modifier.difference(dim.add_modifier);
        }
    }
    let used: usize = spans.iter().map(Span::width).sum();
    spans.push(Span::raw(
        " ".repeat(usize::from(width).saturating_sub(used)),
    ));
    Line::from(spans).style(t.cursor)
}

/// The glyph after the gutter that shows `e`'s mark, in its style; the spinner turns with
/// `frame`, offset per entry.
fn mark_glyph(e: &EntryData, t: &Theme, frame: u64) -> (char, Style) {
    match e.mark {
        Mark::Scanning | Mark::Rescanning => (spinner(frame + (e.dev_ino.1 % 10)), t.fg_dim),
        Mark::Stalled => ('~', t.status_warn),
        Mark::Error => ('!', t.status_error),
        Mark::OtherFs | Mark::Network | Mark::Kernel => ('>', t.fg_dim),
        Mark::DupMount => ('=', t.fg_dim),
        Mark::ErrorsInside => ('.', t.status_error),
        Mark::Deleting => ('x', t.status_warn),
        Mark::Incomplete => ('?', t.status_warn),
        Mark::None => (' ', Style::new()),
    }
}

/// What a row is drawn from besides its entry.
struct Row {
    /// The entry's index in the snapshot.
    index: usize,
    /// The animation frame, for the spinner.
    frame: u64,
    /// The largest size among the rows, a full bar.
    max: u64,
    /// The directory's total, for the share.
    total: u64,
    change: Option<RowNote>,
}

/// The row of an entry in the columns `cols`. A ghost (a deleted entry kept in its place until
/// the next re-sort) is struck through and dimmed, and says so.
fn entry_line(app: &App, row: &Row, cols: &[(Col, u16)]) -> Line<'static> {
    let t = &app.theme;
    let e = &app.snap.entries[row.index];
    let mut spans = Vec::new();
    let mut notes = Vec::new();
    let mut prev = None;
    for &(c, w) in cols {
        spans.push(Span::raw(" ".repeat(c.gap(prev).into())));
        prev = Some(c);
        match c {
            Col::Gutter => {
                let (glyph, gstyle) = mark_glyph(e, t, row.frame);
                // The gutter cell takes the mark's style where the gutter sets none of its own.
                let gutter = marks::gutter(app, e);
                spans.push(Span::styled(gutter.content, gstyle.patch(gutter.style)));
                spans.push(Span::styled(glyph.to_string(), gstyle));
            }
            Col::Size => spans.push(right(human(e.size), w, size_style(e, t))),
            Col::Bar => {
                let frac = motion::bar(app, e, ratio(e.size, row.max));
                spans.push(Span::styled(bar(frac, w.into()), t.bar_fill));
            }
            // A ghost's size is no longer part of the total: its share would be meaningless.
            Col::Share if e.ghost => spans.push(right(String::new(), w, Style::new())),
            Col::Share => {
                let share = format!("{:.1}%", percent(e.size, row.total));
                spans.push(right(share, w, Style::new()));
            }
            // The data shared between entries has no items of its own.
            Col::Items if e.pseudo && !e.ghost => spans.push(right(String::new(), w, t.fg_dim)),
            Col::Items => spans.push(right(count(e.items), w, t.fg_dim)),
            Col::Mtime => {
                let day = (e.mtime > 0 && !e.pseudo).then(|| dfdu_store::history::day(e.mtime));
                spans.push(left(day.unwrap_or_default(), w, t.fg_dim));
            }
            Col::Delta => spans.push(match row.change {
                Some(n) if n.grew() => right(n.short(), w, t.status_warn),
                Some(n) => right(n.short(), w, t.fg_dim),
                None => right(String::new(), w, Style::new()),
            }),
            Col::Shared => {
                let flag = if e.shared_out > 0 { "↔" } else { " " };
                spans.push(Span::styled(flag, t.fg_dim));
            }
            Col::Category => {
                let rules = app.snap.rules.as_ref();
                // Inside a match, rows show its category dim: the match is the directory above.
                let (text, style) = match e.tag.zip(rules).map(|(g, r)| (g, g.category(r))) {
                    Some((g, (name, _))) if g.inherited => (name, t.fg_dim),
                    Some((_, (name, slot))) => (name, t.cat[slot]),
                    None => ("", Style::new()),
                };
                spans.push(left(text.to_owned(), w, style));
            }
            Col::Name => {
                let (name, after) = name::cell(app, row.index, w.into());
                spans.extend(name);
                notes = after;
            }
        }
    }
    if e.ghost {
        let struck = t.fg_dim.add_modifier(Modifier::CROSSED_OUT);
        for s in &mut spans {
            s.style = s.style.patch(struck);
        }
    }
    spans.extend(notes);
    Line::from(spans)
}

/// The style of `e`'s size: dim when it can still grow, or is only a lower bound.
fn size_style(e: &EntryData, t: &Theme) -> Style {
    match e.mark {
        Mark::Scanning
        | Mark::Rescanning
        | Mark::Stalled
        | Mark::Incomplete
        | Mark::Error
        | Mark::ErrorsInside => t.fg_dim,
        _ => Style::new(),
    }
}

#[cfg(test)]
mod tests;
