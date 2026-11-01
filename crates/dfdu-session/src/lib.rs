//! What a front end shows besides drawing and input, with no terminal or window types: the
//! directory snapshot copied out of the engine model (`snapshot`), the directories tree mode
//! expands (`tree`), the order of the rows (`order`) and the fuzzy matching of what is typed
//! (`fuzzy`).

pub mod fuzzy;
pub mod order;
pub mod snapshot;
#[cfg(any(test, feature = "test-support"))]
#[doc(hidden)]
pub mod test_support;
pub mod tree;
