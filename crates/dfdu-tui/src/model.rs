//! What every view reads from outside the UI: the directory snapshot (`snapshot`, in
//! `dfdu-session`) and the features' extras (`extras`) copied from the engine model, the scan
//! history (`history_link`) with what grew since the last scan (`growth`), the scan cache
//! (`cache_link`), and the mounts and links the scan did not enter, looked up on this machine
//! (`outside`). The history, growth, cache and lookups are in `dfdu-session`; their modules here read
//! them with the app. Two features read on their own: the duplicates comparison reads the
//! files' contents (`features::dupes`) and the disk picker lists the disks (`features::picker`).

pub(crate) use dfdu_session::cache_link;
#[cfg(test)]
pub(crate) mod extras;
pub(crate) mod growth;
pub(crate) mod outside;

pub(crate) use dfdu_session::snapshot;
