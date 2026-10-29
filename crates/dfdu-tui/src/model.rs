//! What the browser reads from outside the UI: the directory snapshot (`snapshot`) and the
//! features' extras (`extras`) copied from the engine model, the scan history (`history_link`)
//! with what grew since the last scan (`growth`), the scan cache (`cache_link`), and the mounts
//! and links the scan did not enter, looked up on this machine (`outside`).

pub(crate) mod cache_link;
pub(crate) mod extras;
pub(crate) mod growth;
pub(crate) mod history_link;
pub(crate) mod outside;
pub(crate) mod snapshot;
