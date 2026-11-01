//! Input besides the action keys (`app::keys`): the single-line text input (`line`), the mouse
//! (`mouse`) and the fuzzy matching of what is typed (`fuzzy`, in `dfdu-session`).

pub(crate) mod line;
pub(crate) mod mouse;

pub(crate) use dfdu_session::fuzzy;
