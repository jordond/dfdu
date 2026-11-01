//! The state of the list's rows: which are marked (`marks`), which the name filter hides
//! (`filter`), their order (`order`, in `dfdu-session`), how they move (`motion`), and which
//! directories tree mode expands (`tree`). The rows shown are the session's
//! (`dfdu_session::state`); the scroll is the app's.

pub(crate) mod filter;
pub(crate) mod marks;
pub(crate) mod motion;
pub(crate) mod tree;

pub(crate) use dfdu_session::order;
