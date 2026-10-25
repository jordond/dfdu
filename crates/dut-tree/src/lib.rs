//! The tree of scanned nodes.
//!
//! Only the applier (`dut-engine`) mutates a [`Tree`]; everything else reads it under a read
//! lock or holds [`NodeRef`]s. See `docs/data-pipeline.md` §4.1 and §6.

mod check;
mod meta;
mod tree;

pub use check::Violation;
pub use meta::{ErrKind, Kind, Meta, Totals};
pub use tree::{DirRec, DirState, FreedLink, Ix, NodeFlags, NodeRef, Tree};
