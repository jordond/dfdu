//! The action registry ([`dfdu_session::actions`]) read with the terminal's keys: the action a
//! key runs, the keys an action is shown with, and running it.

mod act;
mod lookup;

pub use act::run;
pub use dfdu_session::actions::{ACTIONS, Action, ActionInfo, Group, offered};
#[cfg(test)]
pub(crate) use lookup::parse;
pub use lookup::{action_for, cursor_after, first_key, half_page};

/// The status line after a key that deletes, in a read-only browser.
pub const READ_ONLY: &str = "read-only: nothing is deleted";

#[cfg(test)]
mod tests;
