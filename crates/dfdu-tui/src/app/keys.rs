//! The action registry ([`dfdu_session::actions`]) read with the terminal's keys: the action a
//! key runs, the keys an action is shown with, and running it.

mod act;
mod lookup;

pub use act::run;
pub use dfdu_session::actions::{ACTIONS, Action, ActionInfo, Group, first_key, offered};
#[cfg(test)]
pub(crate) use lookup::parse;
pub use lookup::{action_for, cursor_after, half_page};

pub use dfdu_session::state::READ_ONLY;

#[cfg(test)]
mod tests;
