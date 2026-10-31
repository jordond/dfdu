//! What the TUI and the headless commands show of the model alike: sizes, problems, subvolume
//! labels, the matches of the rules listed and which are recommended for deletion, the mounts
//! the scan did not enter and the rule locations behind links out of it. Reads of the model
//! only; nothing here runs on the applier.

mod listed;
mod notes;
mod outside;
mod recommend;
mod sizes;

pub use listed::{Listed, listed};
pub use notes::{ErrorItem, ErrorList, errors, errors_below, problem, subvol_label};
pub use outside::{
    LIST_MAX, NotEntered, describe, home_dir, linked_note, more_links, not_entered,
    not_entered_lines, rel_text, tilde,
};
pub use recommend::{Verdict, verdict};
pub use sizes::{SizeMode, human};
