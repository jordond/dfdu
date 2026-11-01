//! The terminal's keys for a [`LineInput`]: each maps to a [`TextOp`], or to none (`↑`, for
//! example) and the caller may use it.

use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use super::line::{Edit, LineInput, TextOp};

/// The [`TextOp`] of `k`; `None` for a key the input does not handle.
pub fn op(k: &KeyEvent) -> Option<TextOp> {
    let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
    Some(match k.code {
        KeyCode::Enter => TextOp::Submit,
        KeyCode::Esc => TextOp::Cancel,
        KeyCode::Char('u') if ctrl => TextOp::Clear,
        KeyCode::Char('w') if ctrl => TextOp::DeleteWord,
        KeyCode::Char(c) if !ctrl && !k.modifiers.contains(KeyModifiers::ALT) => TextOp::Insert(c),
        KeyCode::Backspace => TextOp::Backspace,
        KeyCode::Delete => TextOp::Delete,
        KeyCode::Left => TextOp::Left,
        KeyCode::Right => TextOp::Right,
        KeyCode::Home => TextOp::Home,
        KeyCode::End => TextOp::End,
        _ => return None,
    })
}

/// Applies `k` to `input`: [`Edit::Ignored`] when it is no [`TextOp`].
pub fn edit(input: &mut LineInput, k: &KeyEvent) -> Edit {
    op(k).map_or(Edit::Ignored, |op| input.apply(op))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(code: KeyCode, modifiers: KeyModifiers) -> Option<TextOp> {
        op(&KeyEvent::new(code, modifiers))
    }

    #[test]
    fn keys_map_to_text_ops() {
        let none = KeyModifiers::NONE;
        let ctrl = KeyModifiers::CONTROL;
        assert_eq!(key(KeyCode::Char('a'), none), Some(TextOp::Insert('a')));
        assert_eq!(
            key(KeyCode::Char('A'), KeyModifiers::SHIFT),
            Some(TextOp::Insert('A'))
        );
        assert_eq!(key(KeyCode::Char('u'), ctrl), Some(TextOp::Clear));
        assert_eq!(key(KeyCode::Char('w'), ctrl), Some(TextOp::DeleteWord));
        assert_eq!(key(KeyCode::Char('x'), ctrl), None);
        assert_eq!(key(KeyCode::Char('x'), KeyModifiers::ALT), None);
        assert_eq!(key(KeyCode::Enter, none), Some(TextOp::Submit));
        assert_eq!(key(KeyCode::Esc, none), Some(TextOp::Cancel));
        assert_eq!(key(KeyCode::Backspace, none), Some(TextOp::Backspace));
        assert_eq!(key(KeyCode::Delete, none), Some(TextOp::Delete));
        assert_eq!(key(KeyCode::Home, none), Some(TextOp::Home));
        assert_eq!(key(KeyCode::End, none), Some(TextOp::End));
        assert_eq!(key(KeyCode::Left, none), Some(TextOp::Left));
        assert_eq!(key(KeyCode::Right, none), Some(TextOp::Right));
        assert_eq!(key(KeyCode::Up, none), None);
    }

    #[test]
    fn a_key_that_is_no_text_op_is_ignored() {
        let mut input = LineInput::default();
        let up = KeyEvent::new(KeyCode::Up, KeyModifiers::NONE);
        assert_eq!(edit(&mut input, &up), Edit::Ignored);
        let a = KeyEvent::new(KeyCode::Char('a'), KeyModifiers::NONE);
        assert_eq!(edit(&mut input, &a), Edit::Changed);
        assert_eq!(input.text(), "a");
    }
}
