//! A single-line text input, for the filter, the palette and typed confirmations. It is edited
//! by [`TextOp`]s, which a front end maps its keys or text events to, and the front end draws it.

/// One editing operation on a [`LineInput`]: what a key or a text field event asks for.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum TextOp {
    /// Types a character at the cursor.
    Insert(char),
    /// Deletes the character before the cursor.
    Backspace,
    /// Deletes the character at the cursor.
    Delete,
    Left,
    Right,
    Home,
    End,
    /// Deletes the blanks before the cursor and the word before them.
    DeleteWord,
    /// Deletes the whole text.
    Clear,
    /// Done with the input (`Enter`).
    Submit,
    /// Leaves the input (`Esc`).
    Cancel,
}

/// What a [`TextOp`] did to a [`LineInput`].
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
    /// A key that is no [`TextOp`] (for example `↑`): the caller may use it.
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

    /// Applies `op` and says what it did.
    pub fn apply(&mut self, op: TextOp) -> Edit {
        match op {
            TextOp::Submit => Edit::Submit,
            TextOp::Cancel => Edit::Cancel,
            TextOp::Clear => {
                self.clear();
                Edit::Changed
            }
            TextOp::DeleteWord => {
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
            TextOp::Insert(c) => {
                let at = self.byte(self.cursor);
                self.text.insert(at, c);
                self.cursor += 1;
                Edit::Changed
            }
            TextOp::Backspace if self.cursor > 0 => {
                let at = self.byte(self.cursor - 1);
                self.text.remove(at);
                self.cursor -= 1;
                Edit::Changed
            }
            TextOp::Delete if self.cursor < self.len() => {
                let at = self.byte(self.cursor);
                self.text.remove(at);
                Edit::Changed
            }
            TextOp::Backspace | TextOp::Delete => Edit::Moved,
            TextOp::Left => {
                self.cursor = self.cursor.saturating_sub(1);
                Edit::Moved
            }
            TextOp::Right => {
                self.cursor = (self.cursor + 1).min(self.len());
                Edit::Moved
            }
            TextOp::Home => {
                self.cursor = 0;
                Edit::Moved
            }
            TextOp::End => {
                self.cursor = self.len();
                Edit::Moved
            }
        }
    }

    /// The text before the cursor and the text after it, for drawing the cursor between them.
    pub fn split(&self) -> (&str, &str) {
        self.text.split_at(self.byte(self.cursor))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn typed(s: &str) -> LineInput {
        let mut input = LineInput::default();
        for c in s.chars() {
            assert_eq!(input.apply(TextOp::Insert(c)), Edit::Changed);
        }
        input
    }

    #[test]
    fn typing_and_editing() {
        let mut input = typed("helo");
        assert_eq!((input.text(), input.cursor()), ("helo", 4));
        input.apply(TextOp::Left);
        input.apply(TextOp::Insert('l'));
        assert_eq!((input.text(), input.cursor()), ("hello", 4));
        input.apply(TextOp::Home);
        assert_eq!(input.apply(TextOp::Backspace), Edit::Moved);
        assert_eq!(input.apply(TextOp::Delete), Edit::Changed);
        assert_eq!(input.text(), "ello");
        input.apply(TextOp::End);
        assert_eq!(input.apply(TextOp::Delete), Edit::Moved);
        input.apply(TextOp::Backspace);
        assert_eq!((input.text(), input.cursor()), ("ell", 3));
        input.apply(TextOp::Left);
        assert_eq!(input.split(), ("el", "l"));
        assert_eq!(input.apply(TextOp::Submit), Edit::Submit);
        assert_eq!(input.apply(TextOp::Cancel), Edit::Cancel);
        assert_eq!(
            input.text(),
            "ell",
            "Submit and Cancel leave the text to the caller"
        );
    }

    #[test]
    fn clear_and_delete_word() {
        let mut input = typed("sort by  size");
        assert_eq!(input.apply(TextOp::DeleteWord), Edit::Changed);
        assert_eq!(input.text(), "sort by  ");
        input.apply(TextOp::DeleteWord);
        assert_eq!(input.text(), "sort ");
        // In the middle: only the word before the cursor goes.
        let mut input = typed("one two three");
        for _ in 0..="three".len() {
            input.apply(TextOp::Left);
        }
        input.apply(TextOp::DeleteWord);
        assert_eq!((input.text(), input.cursor()), ("one  three", 4));
        assert_eq!(input.apply(TextOp::Clear), Edit::Changed);
        assert!(input.is_empty());
        assert_eq!(input.cursor(), 0);
    }

    #[test]
    fn cursor_moves_by_chars_not_bytes() {
        let mut input = typed("naïve 日本");
        assert_eq!(input.cursor(), 8);
        input.apply(TextOp::Left);
        input.apply(TextOp::Backspace);
        assert_eq!(input.text(), "naïve 本");
        for _ in 0..4 {
            input.apply(TextOp::Left);
        }
        input.apply(TextOp::Delete);
        assert_eq!(input.text(), "nave 本");
        input.apply(TextOp::Insert('ï'));
        assert_eq!((input.text(), input.cursor()), ("naïve 本", 3));
        assert_eq!(input.split(), ("naï", "ve 本"));
        input.set("ü");
        assert_eq!(input.cursor(), 1);
        input.apply(TextOp::Right);
        assert_eq!(input.cursor(), 1);
        assert_eq!(input.split(), ("ü", ""));
    }
}
