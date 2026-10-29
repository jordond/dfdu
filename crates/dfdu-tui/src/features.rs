//! The views and panels beside the list, each with its own state, keys and drawing: the views
//! `treemap` (`2`), `cleanup` (`3`), `dupes` (`4`) and `largest` (`5`), the `details` pane, the
//! `debug` panel and the disk `picker`; and `selected_path`, which hands the selected path to
//! the clipboard and the file manager.

pub(crate) mod cleanup;
pub(crate) mod debug;
pub(crate) mod details;
pub(crate) mod dupes;
pub(crate) mod largest;
pub(crate) mod picker;
pub(crate) mod selected_path;
pub(crate) mod treemap;
