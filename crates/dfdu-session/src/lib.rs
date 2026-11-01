//! What a front end shows besides drawing and input, with no terminal or window types: the
//! directory snapshot copied out of the engine model (`snapshot`), the directories tree mode
//! expands (`tree`), the order of the rows (`order`) and the fuzzy matching of what is typed
//! (`fuzzy`). From outside the UI: the scan history (`history_link`) with what grew since the
//! last scan (`growth`), the mounts and links the scan did not enter (`outside`), and the file
//! manager that shows a path (`file_manager`). What is opened and how it starts (`startup`), and
//! the treemap's layout (`treemap`).

pub mod file_manager;
pub mod fuzzy;
pub mod growth;
pub mod history_link;
pub mod order;
pub mod outside;
pub mod snapshot;
pub mod startup;
#[cfg(any(test, feature = "test-support"))]
#[doc(hidden)]
pub mod test_support;
pub mod tree;
pub mod treemap;
