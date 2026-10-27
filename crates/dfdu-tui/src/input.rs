//! A single-line text input, for the filter, the palette and typed confirmations.

use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::style::Style;
use ratatui::text::Span;

/// What a key did to a [`LineInput`].
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Edit {
    /// The text changed.
    Changed,
    /// Only the cursor moved.
    Moved,
    /// `Enter`.
    Submit,
    /// `Esc`.
    Cancel,
    /// A key the input does not handle (for example `↑`): the caller may use it.
    Ignored,
}

/// The text and a cursor counted in characters, so editing never splits a UTF-8 sequence.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct LineInput {
    text: String,
    cursor: usize,
}

impl LineInput {
    pub fn text(&self) -> &str {
        &self.text
    }

    /// The cursor, in characters from the start.
    #[cfg(test)]
    pub fn cursor(&self) -> usize {
        self.cursor
    }

    pub fn is_empty(&self) -> bool {
        self.text.is_empty()
    }

    /// Replaces the text and puts the cursor at its end.
    pub fn set(&mut self, text: &str) {
        self.text = text.to_string();
        self.cursor = self.len();
    }

    pub fn clear(&mut self) {
        self.text.clear();
        self.cursor = 0;
    }

    fn len(&self) -> usize {
        self.text.chars().count()
    }

    /// Byte offset of character `i` (the end if `i` is past it).
    fn byte(&self, i: usize) -> usize {
        self.text
            .char_indices()
            .nth(i)
            .map_or(self.text.len(), |(b, _)| b)
    }

    pub fn key(&mut self, k: &KeyEvent) -> Edit {
        let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
        match k.code {
            KeyCode::Enter => Edit::Submit,
            KeyCode::Esc => Edit::Cancel,
            KeyCode::Char('u') if ctrl => {
                self.clear();
                Edit::Changed
            }
            KeyCode::Char('w') if ctrl => {
                let before: Vec<char> = self.text.chars().take(self.cursor).collect();
                let mut start = before.len();
                while start > 0 && before[start - 1].is_whitespace() {
                    start -= 1;
                }
                while start > 0 && !before[start - 1].is_whitespace() {
                    start -= 1;
                }
                let (from, to) = (self.byte(start), self.byte(self.cursor));
                self.text.replace_range(from..to, "");
                self.cursor = start;
                Edit::Changed
            }
            KeyCode::Char(c) if !ctrl && !k.modifiers.contains(KeyModifiers::ALT) => {
                let at = self.byte(self.cursor);
                self.text.insert(at, c);
                self.cursor += 1;
                Edit::Changed
            }
            KeyCode::Backspace if self.cursor > 0 => {
                let at = self.byte(self.cursor - 1);
                self.text.remove(at);
                self.cursor -= 1;
                Edit::Changed
            }
            KeyCode::Delete if self.cursor < self.len() => {
                let at = self.byte(self.cursor);
                self.text.remove(at);
                Edit::Changed
            }
            KeyCode::Backspace | KeyCode::Delete => Edit::Moved,
            KeyCode::Left => {
                self.cursor = self.cursor.saturating_sub(1);
                Edit::Moved
            }
            KeyCode::Right => {
                self.cursor = (self.cursor + 1).min(self.len());
                Edit::Moved
            }
            KeyCode::Home => {
                self.cursor = 0;
                Edit::Moved
            }
            KeyCode::End => {
                self.cursor = self.len();
                Edit::Moved
            }
            _ => Edit::Ignored,
        }
    }

    /// The text in `style` with a `▌` in `cursor_style` at the cursor.
    pub fn spans(&self, style: Style, cursor_style: Style) -> Vec<Span<'static>> {
        let at = self.byte(self.cursor);
        let mut spans = Vec::with_capacity(3);
        if at > 0 {
            spans.push(Span::styled(self.text[..at].to_string(), style));
        }
        spans.push(Span::styled("▌", cursor_style));
        if at < self.text.len() {
            spans.push(Span::styled(self.text[at..].to_string(), style));
        }
        spans
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn press(input: &mut LineInput, code: KeyCode) -> Edit {
        input.key(&KeyEvent::new(code, KeyModifiers::NONE))
    }

    fn ctrl(input: &mut LineInput, c: char) -> Edit {
        input.key(&KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL))
    }

    fn typed(s: &str) -> LineInput {
        let mut input = LineInput::default();
        for c in s.chars() {
            assert_eq!(press(&mut input, KeyCode::Char(c)), Edit::Changed);
        }
        input
    }

    #[test]
    fn typing_and_editing() {
        let mut input = typed("helo");
        assert_eq!((input.text(), input.cursor()), ("helo", 4));
        press(&mut input, KeyCode::Left);
        press(&mut input, KeyCode::Char('l'));
        assert_eq!((input.text(), input.cursor()), ("hello", 4));
        press(&mut input, KeyCode::Home);
        assert_eq!(press(&mut input, KeyCode::Backspace), Edit::Moved);
        assert_eq!(press(&mut input, KeyCode::Delete), Edit::Changed);
        assert_eq!(input.text(), "ello");
        press(&mut input, KeyCode::End);
        assert_eq!(press(&mut input, KeyCode::Delete), Edit::Moved);
        press(&mut input, KeyCode::Backspace);
        assert_eq!((input.text(), input.cursor()), ("ell", 3));
        press(&mut input, KeyCode::Left);
        let spans = input.spans(Style::new(), Style::new());
        let shown: Vec<&str> = spans.iter().map(|s| s.content.as_ref()).collect();
        assert_eq!(shown, ["el", "▌", "l"]);
        assert_eq!(press(&mut input, KeyCode::Up), Edit::Ignored);
        assert_eq!(press(&mut input, KeyCode::Enter), Edit::Submit);
        assert_eq!(press(&mut input, KeyCode::Esc), Edit::Cancel);
        assert_eq!(
            input.text(),
            "ell",
            "Enter and Esc leave the text to the caller"
        );
    }

    #[test]
    fn ctrl_u_clears_and_ctrl_w_deletes_a_word() {
        let mut input = typed("sort by  size");
        assert_eq!(ctrl(&mut input, 'w'), Edit::Changed);
        assert_eq!(input.text(), "sort by  ");
        ctrl(&mut input, 'w');
        assert_eq!(input.text(), "sort ");
        // In the middle: only the word before the cursor goes.
        let mut input = typed("one two three");
        for _ in 0.."three".len() + 1 {
            press(&mut input, KeyCode::Left);
        }
        ctrl(&mut input, 'w');
        assert_eq!((input.text(), input.cursor()), ("one  three", 4));
        assert_eq!(ctrl(&mut input, 'u'), Edit::Changed);
        assert!(input.is_empty());
        assert_eq!(input.cursor(), 0);
    }

    #[test]
    fn cursor_moves_by_chars_not_bytes() {
        let mut input = typed("naïve 日本");
        assert_eq!(input.cursor(), 8);
        press(&mut input, KeyCode::Left);
        press(&mut input, KeyCode::Backspace);
        assert_eq!(input.text(), "naïve 本");
        for _ in 0..4 {
            press(&mut input, KeyCode::Left);
        }
        press(&mut input, KeyCode::Delete);
        assert_eq!(input.text(), "nave 本");
        press(&mut input, KeyCode::Char('ï'));
        assert_eq!((input.text(), input.cursor()), ("naïve 本", 3));
        input.set("ü");
        assert_eq!(input.cursor(), 1);
        press(&mut input, KeyCode::Right);
        assert_eq!(input.cursor(), 1);
    }
}
