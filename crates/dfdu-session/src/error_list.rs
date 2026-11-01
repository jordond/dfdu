//! What could not be read ([`ErrorList`]), as a front end lists it with a cursor: the cursor's
//! moves ([`Move`], [`moved`]) and the entry under it to go to ([`entry`]).

use dfdu_engine::ErrorList;

use crate::view::Show;

/// A move of the cursor.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Move {
    Up,
    Down,
    PageUp,
    PageDown,
    First,
    Last,
}

/// Where the cursor at `cursor` goes with `m` in a list of `len` entries, `page` at a time:
/// always on an entry (on the first of none).
pub fn moved(cursor: usize, len: usize, page: usize, m: Move) -> usize {
    let next = match m {
        Move::Up => cursor.saturating_sub(1),
        Move::Down => cursor + 1,
        Move::PageUp => cursor.saturating_sub(page),
        Move::PageDown => cursor + page,
        Move::First => 0,
        Move::Last => len,
    };
    next.min(len.saturating_sub(1))
}

/// The entry under `cursor`, below the scan root, to go to ([`SessionState::go_to`]); `None`
/// in an empty list.
///
/// [`SessionState::go_to`]: crate::SessionState::go_to
pub fn entry(list: &ErrorList, cursor: usize) -> Option<Show> {
    let e = list.items.get(cursor)?;
    Some(Show {
        path: e.path.clone(),
        root: list.root.clone(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_cursor_stays_on_the_entries() {
        assert_eq!(moved(0, 5, 3, Move::Up), 0);
        assert_eq!(moved(4, 5, 3, Move::Down), 4);
        assert_eq!(moved(1, 5, 3, Move::PageDown), 4);
        assert_eq!(moved(4, 5, 3, Move::PageUp), 1);
        assert_eq!(moved(2, 5, 3, Move::First), 0);
        assert_eq!(moved(0, 5, 3, Move::Last), 4);
        assert_eq!(moved(0, 0, 3, Move::Last), 0, "an empty list");
    }
}
