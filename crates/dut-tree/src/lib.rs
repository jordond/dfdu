//! The tree of scanned nodes.
//!
//! Only the applier (`dut-engine`) mutates a [`Tree`]; everything else reads it under a read
//! lock or holds [`NodeRef`]s. See `docs/data-pipeline.md` §4.1 and §6.

mod check;
mod meta;
mod names;
mod pages;
mod tree;

pub use check::Violation;
pub use meta::{ErrKind, Kind, Meta, Subvol, Totals, WinStat, win_attr};
pub use names::{name_bytes, name_bytes_wide, name_os};
pub use pages::Pages;
pub use tree::{Children, DirRec, DirState, FreedLink, Ix, NodeFlags, NodeRef, SEP, Tree};
