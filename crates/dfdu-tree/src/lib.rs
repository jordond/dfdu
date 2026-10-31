//! The tree of scanned nodes.
//!
//! Only the applier (`dfdu-engine`) mutates a [`Tree`]; everything else reads it under a read
//! lock or holds [`NodeRef`]s.

mod check;
mod class;
mod clock;
pub mod filetype;
mod host;
mod ids;
mod meta;
mod names;
mod pages;
mod tree;

pub use check::Violation;
pub use class::{Class, ClassFlags, RuleIx, Safety};
pub use clock::{civil_from_days, date_time, day, days_from_civil, now_secs};
pub use host::{Host, HostPath};
pub use ids::{DirId, JobId};
pub use meta::{ErrKind, Kind, Meta, Subvol, Totals, WinStat, win_attr};
pub use names::{name_bytes, name_bytes_wide, name_os};
pub use pages::Pages;
pub use tree::{
    Children, DirRec, DirState, FreedLink, Ident, Ix, NodeFlags, NodeRef, Protection, SEP, Target,
    Tree, find_path,
};
