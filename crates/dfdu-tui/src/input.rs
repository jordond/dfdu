//! Input besides the action keys (`app::keys`): the single-line text input (`line`, in
//! `dfdu-session`) and its keys (`line_keys`), the mouse (`mouse`) and the fuzzy matching of what
//! is typed (`fuzzy`, in `dfdu-session`).

pub(crate) mod line_keys;
pub(crate) mod mouse;

pub(crate) use dfdu_session::{fuzzy, line};
