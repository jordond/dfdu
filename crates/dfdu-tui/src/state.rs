//! The state of the list's rows: which are marked (`marks`), which the name filter hides
//! (`filter`), their order (`order`), how they move (`motion`), and which directories tree
//! mode expands (`tree`).

pub(crate) mod filter;
pub(crate) mod marks;
pub(crate) mod motion;
pub(crate) mod order;
pub(crate) mod tree;
