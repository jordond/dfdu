//! What a front end shows besides drawing and input, with no terminal or window types: the
//! state of a browser (`state`, [`SessionState`]) with the view shown (`view`), its marks and
//! the marks pane (`marks`) and name filter (`filter`), the directory snapshot copied out of
//! the engine model (`snapshot`), the directories tree mode expands (`tree`), the order of the
//! rows (`order`) and the fuzzy matching of what is typed (`fuzzy`). What the features read
//! from the model besides the snapshot (`extras`), and what is left to carry out after a key
//! (`effect`). From outside the UI: the scan history (`history_link`) with what grew since the
//! last scan (`growth`), the scan cache (`cache_link`), the mounts and links the scan did not
//! enter (`outside`), and the file manager that shows a path (`file_manager`). What is opened
//! and how it starts (`startup`). The views: the treemap's layout, depth and lower levels
//! (`treemap`), the cleanup view (`cleanup`), the duplicates comparison with its keep rules and
//! view (`dupes`), and the largest files below a directory (`largest`). The actions with their
//! keys and labels (`actions`), the delete confirmation (`confirm`), the details of an entry
//! (`details`), the debug counters (`debug`), mark by pattern (`mark_pattern`), the text for
//! numbers (`format`), the theme's tokens, built-in themes and theme files (`palette`), and the
//! single-line text input (`line`).

pub mod actions;
pub mod cache_link;
pub mod cleanup;
pub mod confirm;
pub mod debug;
pub mod details;
pub mod dupes;
pub mod effect;
pub mod extras;
pub mod file_manager;
pub mod filter;
pub mod format;
pub mod fuzzy;
pub mod growth;
pub mod history_link;
pub mod largest;
pub mod line;
pub mod mark_pattern;
pub mod marks;
pub mod order;
pub mod outside;
pub mod palette;
pub mod session;
pub mod snapshot;
pub mod startup;
pub mod state;
#[cfg(any(test, feature = "test-support"))]
#[doc(hidden)]
pub mod test_support;
pub mod tree;
pub mod treemap;
pub mod view;

pub use session::{FrontEnd, Session, SessionOptions, Tick};
pub use state::SessionState;
