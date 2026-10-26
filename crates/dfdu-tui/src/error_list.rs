//! The error list overlay: what could not be read, with a cursor; `Enter` goes to the entry.

use ratatui::Frame;
use ratatui::crossterm::event::{KeyCode, KeyEvent};
use ratatui::layout::{Constraint, Rect};
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Clear, Paragraph};

use crate::app::{App, Effect, Overlay};
use crate::theme::count;

/// Cursor keys move, `Enter` (or `→`, `l`) goes to the entry, any other key closes the list.
pub fn key(app: &mut App, k: KeyEvent) -> Vec<Effect> {
    let Overlay::Errors { list, cursor } = &app.overlay else {
        return Vec::new();
    };
    let (n, page) = (list.items.len(), app.list_height.max(1));
    let cursor = *cursor;
    let next = match k.code {
        KeyCode::Up | KeyCode::Char('k') => cursor.saturating_sub(1),
        KeyCode::Down | KeyCode::Char('j') => cursor + 1,
        KeyCode::PageUp => cursor.saturating_sub(page),
        KeyCode::PageDown => cursor + page,
        KeyCode::Home | KeyCode::Char('g') => 0,
        KeyCode::End | KeyCode::Char('G') => n,
        KeyCode::Enter | KeyCode::Right | KeyCode::Char('l') => {
            if let Some(e) = list.items.get(cursor) {
                let (path, root) = (e.path.clone(), list.root.clone());
                app.overlay = Overlay::None;
                app.go_to(&path, &root);
            }
            return Vec::new();
        }
        _ => {
            app.overlay = Overlay::None;
            return Vec::new();
        }
    };
    if let Overlay::Errors { cursor, .. } = &mut app.overlay {
        *cursor = next.min(n.saturating_sub(1));
    }
    Vec::new()
}

/// Draws the list centred in `area` and returns the rectangle it covers.
pub fn draw(f: &mut Frame, app: &App, area: Rect) -> Rect {
    let Overlay::Errors { list, cursor } = &app.overlay else {
        return Rect::default();
    };
    let cursor = *cursor;
    let t = &app.theme;
    let height = area.height.saturating_sub(4).max(3) as usize;
    let rows = list.items.len().min(height.saturating_sub(2)).max(1);
    let top = cursor.saturating_sub(rows - 1);
    let width = area.width.saturating_sub(4) as usize;
    let mut lines: Vec<Line> = list
        .items
        .iter()
        .enumerate()
        .skip(top)
        .take(rows)
        .map(|(i, e)| {
            let rel = e.path.strip_prefix(&list.root).unwrap_or(&e.path);
            let mut path = if rel.as_os_str().is_empty() {
                ".".to_string()
            } else {
                rel.display().to_string()
            };
            if e.is_dir {
                path.push('/');
            }
            let style = if i == cursor {
                t.selected
            } else {
                Style::new()
            };
            let room = width.saturating_sub(e.problem.chars().count() + 4);
            if path.chars().count() > room {
                let keep = room.saturating_sub(1);
                let skip = path.chars().count() - keep;
                path = format!("…{}", path.chars().skip(skip).collect::<String>());
            }
            Line::from(vec![
                Span::styled(format!(" {path}"), style),
                Span::styled(format!("  {}", e.problem), t.error),
            ])
        })
        .collect();
    if list.items.is_empty() {
        lines.push(Line::styled(" Nothing to list.", t.dim));
    }
    let mut foot = String::from(" Enter go to it · Esc close");
    if (list.items.len() as u64) < list.total {
        foot += &format!(" · first {} shown", count(list.items.len() as u64));
    }
    if let Some(h) = list.hint() {
        foot += &format!(" · {h}");
    }
    lines.push(Line::raw(""));
    lines.push(Line::styled(foot, t.dim));
    let title = format!(" {} not read: {} ", count(list.total), list.summary());
    let r = area.centered(
        Constraint::Length(area.width.saturating_sub(4).min(100)),
        Constraint::Length(lines.len() as u16 + 2),
    );
    f.render_widget(Clear, r);
    f.render_widget(
        Paragraph::new(lines).block(Block::bordered().title(title)),
        r,
    );
    r
}
