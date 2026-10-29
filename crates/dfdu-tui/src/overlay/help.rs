//! The help overlay: every action of the registry by group, in two columns when there is room,
//! then how rows keep their order and how the mouse works. `/` searches the keys and labels.

use std::cell::Cell;

use ratatui::Frame;
use ratatui::crossterm::event::{KeyCode, KeyEvent};
use ratatui::layout::{Constraint, Rect};
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Clear, Paragraph};

use crate::app::keys::{self, Action, ActionInfo, Group, first_key, offered};
use crate::app::{App, Effect, Overlay};
use crate::input::fuzzy::{Fuzzy, char_pos};
use crate::input::line::{Edit, LineInput};
use crate::theme::Theme;

/// The groups in the order they are shown, with their headings.
const GROUPS: [(Group, &str); 6] = [
    (Group::Navigate, "Navigate"),
    (Group::Sort, "Sort"),
    (Group::Display, "Display"),
    (Group::Act, "Act"),
    (Group::Panels, "Panels"),
    (Group::Global, "Global"),
];

/// Below the keys, while nothing is searched.
const NOTES: [&str; 4] = [
    "Rows hold their order while you navigate; they re-sort 1.5 s after your last key, or when \
     you press S.",
    "In the delete dialog, p switches between the Trash and deleting permanently, s to staging: \
     renamed aside at once, deleted after a minute. U puts back what was last moved to the Trash \
     or staged.",
    "In the Map view, arrows and hjkl move between tiles, Enter zooms in, and Backspace or < \
     zooms out. Tiles keep their place until the rows re-sort.",
    "Mouse: click selects, double-click opens, the wheel scrolls. Hold Shift to select text in \
     the terminal while the mouse is on (--no-mouse turns it off).",
];

/// The first note: the views by key and name, how to step between them and what each covers.
fn views_note() -> String {
    use crate::app::views::View;
    let names: Vec<String> = (View::ALL.iter())
        .map(|v| format!("{} {}", first_key(v.action()), v.name()))
        .collect();
    let (prev, next) = (first_key(Action::PrevView), first_key(Action::NextView));
    let scope = |whole: bool| {
        let named: Vec<&str> = (View::ALL.iter())
            .filter(|v| v.whole_scan() == whole)
            .map(|v| v.name())
            .collect();
        named.join(", ")
    };
    format!(
        "Views: {}; {prev} and {next} step through them and {} goes back to {}. {} show the \
         current directory; {} the whole scan.",
        names.join(", "),
        first_key(Action::Close),
        View::Files.name(),
        scope(false),
        scope(true),
    )
}

/// After the label of an action without a key.
const PALETTE_ONLY: &str = "(in the palette)";

/// An overlay this wide or wider shows the groups in two columns.
const TWO_COLUMNS_FROM: u16 = 80;
const MAX_WIDTH: u16 = 100;
/// Between the two columns.
const GAP: usize = 2;

/// State of the open help, held in `Overlay::Help`.
#[derive(Clone, Debug, Default)]
pub struct Help {
    search: LineInput,
    /// Typing goes to the search.
    searching: bool,
    /// First line of the keys on screen; drawing keeps it within the content.
    scroll: Cell<usize>,
    /// The last first line the last frame could show, once a frame was drawn.
    max: Cell<Option<usize>>,
    /// The browser is read-only: the actions that delete are left out.
    read_only: bool,
}

/// An action that matches the search, with the matched characters of its keys and its label.
struct Row {
    info: &'static ActionInfo,
    key_hits: Vec<u32>,
    label_hits: Vec<u32>,
}

impl Help {
    /// True while the search line is shown: the search is being typed, or it filters.
    fn prompt_shown(&self) -> bool {
        self.searching || !self.search.is_empty()
    }

    /// True while typing goes to the search.
    pub fn searching(&self) -> bool {
        self.searching
    }

    /// The groups with the actions offered that match the search (all of them without one), in
    /// registry order. A query matches the keys and the label of an action together.
    fn rows(&self) -> Vec<(&'static str, Vec<Row>)> {
        let mut fuzzy = Fuzzy::new(self.search.text());
        let mut hits = Vec::new();
        GROUPS
            .iter()
            .filter_map(|&(group, heading)| {
                let rows: Vec<Row> = offered(self.read_only)
                    .filter(|info| info.group == group)
                    .filter_map(|info| {
                        let keys = char_pos(info.keys.chars().count());
                        fuzzy.indices(&format!("{}  {}", info.keys, info.label), &mut hits)?;
                        let (key_hits, label_hits): (Vec<u32>, Vec<u32>) =
                            hits.iter().partition(|&&i| i < keys);
                        Some(Row {
                            info,
                            key_hits,
                            label_hits: label_hits.iter().map(|i| i - keys - 2).collect(),
                        })
                    })
                    .collect();
                (!rows.is_empty()).then_some((heading, rows))
            })
            .collect()
    }
}

/// Opens the help at the top, without a search.
pub fn open(app: &mut App) {
    app.overlay = Overlay::Help(Help {
        read_only: app.read_only,
        ..Help::default()
    });
}

/// `↑`/`↓`/`PgUp`/`PgDn` scroll; `/` starts a search, which `Enter` keeps and `Esc` clears;
/// `Esc`, `?`, `q`, `F1` and `Enter` close the help. Other keys do nothing.
pub fn key(app: &mut App, k: KeyEvent) -> Vec<Effect> {
    let covered = app.areas.overlay;
    let Overlay::Help(h) = &mut app.overlay else {
        return Vec::new();
    };
    // The lines of keys the last frame showed: the overlay less its border and search line.
    let page = covered
        .map_or(1, |r| {
            usize::from(r.height.saturating_sub(2)).saturating_sub(usize::from(h.prompt_shown()))
        })
        .max(1);
    let top = h.scroll.get();
    let scrolled = match k.code {
        KeyCode::Up => Some(top.saturating_sub(1)),
        KeyCode::Down => Some(top + 1),
        KeyCode::PageUp => Some(top.saturating_sub(page)),
        KeyCode::PageDown => Some(top + page),
        // While a search is typed, Ctrl-U clears it instead.
        _ if !h.searching => keys::half_page(&k, page).map(|n| top.saturating_add_signed(n)),
        _ => None,
    };
    if let Some(top) = scrolled {
        h.scroll.set(h.max.get().map_or(top, |max| top.min(max)));
        return Vec::new();
    }
    if h.searching {
        match h.search.key(&k) {
            Edit::Changed => h.scroll.set(0),
            Edit::Submit => h.searching = false,
            Edit::Cancel => {
                h.search.clear();
                h.searching = false;
                h.scroll.set(0);
            }
            Edit::Moved | Edit::Ignored => {}
        }
        return Vec::new();
    }
    match k.code {
        KeyCode::Char('/') => h.searching = true,
        KeyCode::Esc | KeyCode::Enter | KeyCode::Char('?' | 'q') | KeyCode::F(1) => {
            app.overlay = Overlay::None;
        }
        _ => {}
    }
    Vec::new()
}

/// Draws the help centred in `area` and returns the rectangle it covers.
pub fn draw(f: &mut Frame, app: &App, area: Rect) -> Rect {
    let Overlay::Help(h) = &app.overlay else {
        return Rect::default();
    };
    let t = &app.theme;
    let width = super::width(area, MAX_WIDTH);
    let inner = usize::from(width.saturating_sub(2));
    let groups = h.rows();
    let shown: usize = groups.iter().map(|(_, rows)| rows.len()).sum();
    let mut body = (width >= TWO_COLUMNS_FROM)
        .then(|| two_columns(&groups, inner, t))
        .flatten()
        .unwrap_or_else(|| column(&groups, inner, t));
    if groups.is_empty() {
        body.push(Line::styled(" nothing matches", t.fg_dim));
    }
    if h.search.is_empty() {
        for note in std::iter::once(views_note()).chain(NOTES.map(String::from)) {
            body.push(Line::raw(""));
            body.extend(
                wrap(&note, inner.saturating_sub(2))
                    .into_iter()
                    .map(|part| Line::styled(format!(" {part}"), t.fg_dim)),
            );
        }
    }

    let prompt = usize::from(h.prompt_shown());
    let height = super::height(area, body.len() + prompt, 2);
    let r = area.centered(Constraint::Length(width), Constraint::Length(height));
    let rows = usize::from(height.saturating_sub(2)).saturating_sub(prompt);
    let max = body.len().saturating_sub(rows);
    let top = h.scroll.get().min(max);
    h.scroll.set(top);
    h.max.set(Some(max));

    let mut lines = Vec::with_capacity(rows + prompt);
    if prompt > 0 {
        let mut spans = vec![Span::styled(" /", t.key), Span::raw(" ")];
        if h.searching {
            spans.extend(h.search.spans(Style::new(), t.key));
        } else {
            spans.push(Span::raw(h.search.text().to_owned()));
        }
        spans.push(Span::styled(
            format!("   {shown} of {}", offered(h.read_only).count()),
            t.fg_dim,
        ));
        lines.push(Line::from(spans));
    }
    lines.extend(body.into_iter().skip(top).take(rows));

    let mut block = Block::bordered()
        .border_type(BorderType::Thick)
        .border_style(t.border_focus)
        .title(Span::styled(" Keys ", t.title));
    if !h.prompt_shown() {
        block = block.title(Line::styled(" / search ", t.fg_dim).right_aligned());
    }
    if top < max {
        block = block.title_bottom(Line::styled(" ↓ more ", t.fg_dim).right_aligned());
    }
    f.render_widget(Clear, r);
    f.render_widget(Paragraph::new(lines).block(block), r);
    r
}

/// The groups one below the other, on `width` cells, each with its keys in a column as wide as
/// its longest keys.
fn column(groups: &[(&str, Vec<Row>)], width: usize, t: &Theme) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    for (i, (heading, rows)) in groups.iter().enumerate() {
        if i > 0 {
            lines.push(Line::raw(""));
        }
        lines.push(Line::from(Span::styled(format!(" {heading}"), t.title)));
        let keys = rows
            .iter()
            .map(|r| {
                r.info
                    .keys
                    .chars()
                    .count()
                    .max(first_key(Action::Palette).chars().count())
            })
            .max()
            .unwrap_or(0);
        for row in rows {
            lines.extend(row_lines(row, keys, width, t));
        }
    }
    lines
}

/// The first groups on the left and the others on the right, split where the two columns are
/// closest in height; `None` for fewer than two groups.
fn two_columns(groups: &[(&str, Vec<Row>)], width: usize, t: &Theme) -> Option<Vec<Line<'static>>> {
    let half = width.saturating_sub(GAP) / 2;
    let (left, right) = (1..groups.len())
        .map(|k| (column(&groups[..k], half, t), column(&groups[k..], half, t)))
        .min_by_key(|(l, r)| l.len().max(r.len()))?;
    let blank = Line::raw("");
    Some(
        (0..left.len().max(right.len()))
            .map(|i| {
                let mut spans = left.get(i).unwrap_or(&blank).spans.clone();
                let used: usize = spans.iter().map(Span::width).sum();
                spans.push(Span::raw(" ".repeat(half + GAP - used.min(half))));
                if let Some(r) = right.get(i) {
                    spans.extend(r.spans.iter().cloned());
                }
                Line::from(spans)
            })
            .collect(),
    )
}

/// One action on `width` cells: its keys in a column `keys` wide, then its label, wrapped
/// under itself when it is longer than the room left (less a cell at the end). An action
/// without a key says it is in the palette.
fn row_lines(row: &Row, keys: usize, width: usize, t: &Theme) -> Vec<Line<'static>> {
    let indent = 1 + keys + 2;
    let room = width.saturating_sub(indent + 1).max(8);
    let label = if row.info.keys.is_empty() {
        format!("{} {PALETTE_ONLY}", row.info.label)
    } else {
        row.info.label.to_owned()
    };
    wrap_at(&label, room)
        .into_iter()
        .enumerate()
        .map(|(i, (start, part))| {
            let mut spans = Vec::new();
            if i == 0 && row.info.keys.is_empty() {
                // In the keys column: the first key of the palette, where the action runs.
                let palette = first_key(Action::Palette);
                spans.push(Span::raw(" "));
                spans.push(Span::styled(palette, t.fg_dim));
                spans.push(Span::raw(" ".repeat(indent - 1 - palette.chars().count())));
            } else if i == 0 {
                spans.push(Span::raw(" "));
                spans.extend(highlighted(row.info.keys, &row.key_hits, 0, t.key, t));
                let pad = indent - 1 - row.info.keys.chars().count();
                spans.push(Span::raw(" ".repeat(pad)));
            } else {
                spans.push(Span::raw(" ".repeat(indent)));
            }
            spans.extend(highlighted(part, &row.label_hits, start, Style::new(), t));
            Line::from(spans)
        })
        .collect()
}

/// `text` in `style`, with the characters whose index (counted from `offset`) is in `hits`
/// highlighted as matches.
fn highlighted(
    text: &str,
    hits: &[u32],
    offset: usize,
    style: Style,
    t: &Theme,
) -> Vec<Span<'static>> {
    let mut spans: Vec<Span<'static>> = Vec::new();
    for (i, ch) in text.chars().enumerate() {
        let hit = hits.binary_search(&char_pos(offset + i)).is_ok();
        let s = if hit { style.patch(t.r#match) } else { style };
        match spans.last_mut() {
            Some(last) if last.style == s => last.content.to_mut().push(ch),
            _ => spans.push(Span::styled(ch.to_string(), s)),
        }
    }
    spans
}

/// `text` broken at spaces into parts of at most `width` characters (a longer word is cut).
fn wrap(text: &str, width: usize) -> Vec<&str> {
    wrap_at(text, width).into_iter().map(|(_, s)| s).collect()
}

/// [`wrap`], with the index of the first character of each part in `text`.
fn wrap_at(text: &str, width: usize) -> Vec<(usize, &str)> {
    let width = width.max(1);
    let chars: Vec<(usize, char)> = text.char_indices().collect();
    let byte = |i: usize| chars.get(i).map_or(text.len(), |&(b, _)| b);
    let mut parts = Vec::new();
    let mut start = 0;
    while start < chars.len() {
        let end = (start + width).min(chars.len());
        let cut = if end == chars.len() {
            end
        } else {
            (start + 1..=end)
                .rev()
                .find(|&i| chars[i].1 == ' ')
                .unwrap_or(end)
        };
        parts.push((start, text[byte(start)..byte(cut)].trim_end()));
        start = cut;
        while start < chars.len() && chars[start].1 == ' ' {
            start += 1;
        }
    }
    parts
}

#[cfg(test)]
mod tests;
