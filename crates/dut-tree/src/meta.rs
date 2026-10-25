use std::io;

/// File type of an entry, as reported without following symlinks.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub enum Kind {
    File,
    Dir,
    Symlink,
    Other,
}

/// Metadata of one entry, as produced by a scanner backend.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct Meta {
    pub kind: Kind,
    /// File length in bytes.
    pub apparent: u64,
    /// Bytes allocated on disk (`st_blocks * 512` or the platform equivalent).
    pub alloc: u64,
    /// Modification time, seconds since the Unix epoch.
    pub mtime: i64,
    pub dev: u64,
    pub ino: u64,
    pub nlink: u32,
}

impl Meta {
    /// A file with `nlink > 1` takes part in hard-link charging.
    pub fn is_hardlink(&self) -> bool {
        self.kind != Kind::Dir && self.nlink > 1
    }
}

/// Error classes recorded on entries and directories.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub enum ErrKind {
    PermissionDenied,
    NotFound,
    /// Raw OS error code.
    Os(i32),
    /// Abandoned after a filesystem call did not return.
    Stalled,
    Other,
}

impl From<&io::Error> for ErrKind {
    fn from(e: &io::Error) -> Self {
        match e.kind() {
            io::ErrorKind::PermissionDenied => ErrKind::PermissionDenied,
            io::ErrorKind::NotFound => ErrKind::NotFound,
            _ => match e.raw_os_error() {
                Some(code) => ErrKind::Os(code),
                None => ErrKind::Other,
            },
        }
    }
}

/// Aggregated sizes and counts of a node and everything below it.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub struct Totals {
    pub apparent: u64,
    pub alloc: u64,
    /// Number of nodes, the node itself included.
    pub items: u64,
    /// Number of nodes with an error.
    pub errors: u64,
}

impl Totals {
    pub fn add(&mut self, o: &Totals) {
        self.apparent += o.apparent;
        self.alloc += o.alloc;
        self.items += o.items;
        self.errors += o.errors;
    }

    /// Subtracts `o`. Underflow is a bookkeeping bug: it panics in debug builds and
    /// saturates in release builds, where the invariant check reports it.
    pub fn sub(&mut self, o: &Totals) {
        debug_assert!(
            self.apparent >= o.apparent
                && self.alloc >= o.alloc
                && self.items >= o.items
                && self.errors >= o.errors,
            "totals underflow: {self:?} - {o:?}"
        );
        self.apparent = self.apparent.saturating_sub(o.apparent);
        self.alloc = self.alloc.saturating_sub(o.alloc);
        self.items = self.items.saturating_sub(o.items);
        self.errors = self.errors.saturating_sub(o.errors);
    }
}
