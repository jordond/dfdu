//! The empty state of a view: a box in the middle of the view with the view's name, what the
//! view does in a sentence or two, and the keys to start or to go back, in place of a blank or
//! a one-line message. Map, Cleanup, Duplicates and Largest show one when they have nothing to
//! show, and Duplicates before it compared anything.

use ratatui::Frame;
use ratatui::layout::{Constraint, Rect};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Clear, Paragraph, Wrap};

use super::cells;
use crate::theme::Theme;

/// The widest the box gets.
const MAX_WIDTH: u16 = 76;
/// Between two keys on the keys line.
const KEY_GAP: &str = "      ";

/// What an empty state says.
pub struct Empty<'a> {
    /// The view's name, on the box's border.
    pub title: &'a str,
    /// Paragraphs: what the view does, then why it is empty.
    pub text: Vec<String>,
    /// The keys to start or to go back, with what they do.
    pub keys: Vec<(&'a str, &'a str)>,
}

/// Lines `text` takes wrapped at `width` cells, about as the paragraph wraps it.
fn wrapped(text: &str, width: usize) -> usize {
    let mut lines = 1;
    let mut used = 0;
    for word in text.split(' ') {
        let w = Span::raw(word).width();
        if used > 0 && used + 1 + w > width {
            lines += 1;
            used = w;
        } else {
            used += w + usize::from(used > 0);
        }
    }
    lines
}

/// Draws `e` in a box centred in `area`.
pub fn draw(f: &mut Frame, t: &Theme, area: Rect, e: &Empty) {
    let width = area.width.saturating_sub(4).min(MAX_WIDTH);
    if width < 10 || area.height < 3 {
        return;
    }
    let inner = usize::from(width).saturating_sub(4);
    let mut lines = Vec::new();
    let mut height = 0;
    for (i, p) in e.text.iter().enumerate() {
        if i > 0 {
            lines.push(Line::raw(""));
            height += 1;
        }
        lines.push(Line::styled(
            p.clone(),
            if i == 0 { t.fg } else { t.fg_dim },
        ));
        height += wrapped(p, inner);
    }
    if !e.keys.is_empty() {
        let mut spans = Vec::new();
        for (i, (key, does)) in e.keys.iter().enumerate() {
            if i > 0 {
                spans.push(Span::raw(KEY_GAP));
            }
            spans.push(Span::styled(*key, t.key));
            spans.push(Span::styled(format!("  {does}"), t.fg_dim));
        }
        lines.push(Line::raw(""));
        lines.push(Line::from(spans));
        height += 2;
    }
    let box_h = cells(height + 2).min(area.height);
    let r = area.centered(Constraint::Length(width), Constraint::Length(box_h));
    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(t.border)
        .title(Span::styled(format!(" {} ", e.title), t.title))
        .padding(ratatui::widgets::Padding::horizontal(1));
    f.render_widget(Clear, r);
    f.render_widget(
        Paragraph::new(lines).block(block).wrap(Wrap { trim: true }),
        r,
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wrapping_counts_the_lines() {
        assert_eq!(wrapped("one two three", 20), 1);
        assert_eq!(wrapped("one two three", 7), 2);
        assert_eq!(wrapped("one two three", 3), 3);
    }
}
