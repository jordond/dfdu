//! The colour a tile takes, as a theme token: its category, its age band or its top folder
//! ([`ColorBy`]). Each front end turns the token into its own style.

use super::{ColorBy, Tile, color_shown};
use crate::palette::{Category, Token};
use crate::state::SessionState;

/// Age bands for [`ColorBy::Age`]: their upper bound in days and name.
pub const AGES: [(i64, &str); 4] = [
    (7, "week"),
    (90, "3 months"),
    (365, "year"),
    (i64::MAX, "older"),
];

/// The age band of a time modified `mtime` at `now`, both in seconds since the epoch: an index
/// into [`AGES`]. A time ahead of the clock is new.
#[must_use]
pub fn age_band(mtime: i64, now: i64) -> usize {
    let days = now.saturating_sub(mtime) / 86_400;
    AGES.iter()
        .position(|&(d, _)| days < d)
        .unwrap_or(AGES.len() - 1)
}

/// The colour of age band `band`: new is good, old is dim.
#[must_use]
pub fn age_token(band: usize) -> Token {
    [Token::StatusOk, Token::Fg, Token::StatusWarn, Token::FgDim][band.min(AGES.len() - 1)]
}

/// The token of category colour slot `slot`.
fn slot_token(slot: usize) -> Token {
    Category::ALL
        .get(slot)
        .map_or(Token::CatOther, |c| c.token())
}

/// The token tile `i` of `tiles` takes its colour from, and its category colour slot when it is
/// coloured by one, at `today` (seconds since the epoch). In category colouring a tile takes its
/// match's category, a directory without one the category most of it is in, and anything else
/// its parent's colour; by top folder every tile takes its top-level tile's colour. The tile of
/// the entries merged takes its parent's colour.
#[must_use]
pub fn base(s: &SessionState, tiles: &[Tile], i: usize, today: i64) -> (Token, Option<usize>) {
    let tile = &tiles[i];
    let parent = || tile.up.map(|u| base(s, tiles, u, today));
    if tile.small > 0 {
        return parent().unwrap_or((Token::FgDim, None));
    }
    match color_shown(s) {
        ColorBy::Folder => parent().unwrap_or((slot_token(tile.top % Category::ALL.len()), None)),
        ColorBy::Age => (age_token(age_band(tile.mtime, today)), None),
        ColorBy::Category => {
            let slot = match (&s.snap.rules, tile.tag) {
                (Some(rules), Some(tag)) => Some(tag.category(rules).1),
                _ => tile.dominant,
            };
            match slot {
                Some(slot) => (slot_token(slot), Some(slot)),
                None => parent().unwrap_or((Token::CatOther, None)),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const DAY: i64 = 86_400;

    #[test]
    fn ages_fall_in_their_band() {
        let now = 1_792_324_800;
        let bands: Vec<usize> = [3, 30, 200, 2_000]
            .iter()
            .map(|days| age_band(now - days * DAY, now))
            .collect();
        assert_eq!(bands, [0, 1, 2, 3]);
        assert_eq!(
            age_band(now + DAY, now),
            0,
            "a time ahead of the clock is new"
        );
        assert_eq!(age_token(0), Token::StatusOk);
        assert_eq!(age_token(3), Token::FgDim);
    }

    #[test]
    fn slots_name_the_category_tokens() {
        assert_eq!(slot_token(Category::Build as usize), Token::CatBuild);
        assert_eq!(slot_token(99), Token::CatOther);
    }
}
