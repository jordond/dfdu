//! Scan ids. The scanner (`dfdu-scan`) allocates them and the tree records them per directory
//! ([`crate::DirRec`]), so they live here, below both.

/// A scan job: one scan of a root, numbered by the scanner.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash, PartialOrd, Ord)]
pub struct JobId(pub u32);

/// Per-job directory id, allocated by the scanner when it creates a task.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash, PartialOrd, Ord)]
pub struct DirId(pub u32);

impl DirId {
    /// The job root.
    pub const ROOT: DirId = DirId(0);
}
