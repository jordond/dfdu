//! What a key or a click leaves to be carried out after the state changed: commands for the
//! engine, reads of the model, and what only the front end can do (the clipboard, a file
//! manager).

use std::path::PathBuf;

use dfdu_engine::Command;

#[derive(Debug)]
pub enum Effect {
    Send(Command),
    /// Read the error list from the engine and show it.
    ShowErrors,
    /// Read the duplicate candidates and start comparing them (`dupes::begin`).
    FindDupes,
    /// Put the text on the clipboard.
    Copy(String),
    /// Show the path in the file manager, without waiting for it.
    Open {
        path: PathBuf,
        is_dir: bool,
    },
}
