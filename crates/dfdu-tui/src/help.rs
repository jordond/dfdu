//! The help overlay: the keys, and how rows keep their order.

use ratatui::Frame;
use ratatui::crossterm::event::KeyEvent;
use ratatui::layout::{Constraint, Rect};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Clear, Paragraph};

use crate::app::{App, Effect, Overlay};

/// Any key closes the help.
pub fn key(app: &mut App, _k: KeyEvent) -> Vec<Effect> {
    app.overlay = Overlay::None;
    Vec::new()
}

/// Draws the help centred in `area` and returns the rectangle it covers.
pub fn draw(f: &mut Frame, app: &App, area: Rect) -> Rect {
    let keys = [
        ("↑ ↓  k j", "move"),
        ("→ l Enter", "open directory"),
        ("← h Backspace", "parent directory"),
        ("PgUp PgDn g G", "page, first, last"),
        (
            "s n C M",
            "sort by size, name, items, mtime (again: reverse)",
        ),
        ("S", "re-sort now"),
        ("a", "toggle apparent size / disk usage"),
        ("u", "toggle disk usage like du (shared data once per file)"),
        ("e", "hide or show hidden files"),
        ("r", "rescan the selected directory"),
        ("x", "skip directories that are not responding"),
        ("!", "list what could not be read, and go to it"),
        ("d", "move to Trash"),
        ("D", "delete permanently (type delete)"),
        ("q Ctrl-C", "quit"),
    ];
    let mut lines: Vec<Line> = keys
        .iter()
        .map(|(k, d)| {
            Line::from(vec![
                Span::styled(format!(" {k:<15}"), app.theme.key),
                Span::raw(*d),
            ])
        })
        .collect();
    lines.push(Line::raw(""));
    lines.push(Line::styled(
        " Rows hold their order while you navigate; they re-sort",
        app.theme.dim,
    ));
    lines.push(Line::styled(
        " 1.5 s after your last key, or when you press S.",
        app.theme.dim,
    ));
    let r = area.centered(
        Constraint::Length(72),
        Constraint::Length(lines.len() as u16 + 2),
    );
    f.render_widget(Clear, r);
    f.render_widget(
        Paragraph::new(lines).block(Block::bordered().title(" Keys ")),
        r,
    );
    r
}
