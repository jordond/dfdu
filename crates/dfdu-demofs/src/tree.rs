//! The tree the filesystem serves: inodes in a vector, node 0 the root, changed in place by the
//! operations the kernel forwards. Names are bytes, as the kernel passes them.

use std::time::Duration;

mod data;

pub(crate) use data::{mix, seed_of};

/// Size reported for a directory, and what it allocates.
pub const DIR_SIZE: u64 = 4096;

/// What an operation failed with; the filesystem answers the matching errno.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FsError {
    NoEnt,
    NotDir,
    IsDir,
    NotEmpty,
    Exist,
    Inval,
    Denied,
    Io,
    TooBig,
}

/// A fault a fixture entry is given: a directory that cannot be listed, a file that cannot be
/// stat'ed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Fault {
    /// `EACCES`.
    Denied,
    /// `EIO`.
    Io,
}

impl From<Fault> for FsError {
    fn from(f: Fault) -> FsError {
        match f {
            Fault::Denied => FsError::Denied,
            Fault::Io => FsError::Io,
        }
    }
}

/// A file's bytes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Data {
    /// The first `len` bytes generated from `seed` when read (equal seeds read the same
    /// bytes), zeros after them: a file grown by `truncate` reads zeros in the new part.
    Seeded { seed: u64, len: u64 },
    /// Written or given in the fixture.
    Bytes(Vec<u8>),
}

/// A name in a directory. `cookie` is its position in the listing: entries are added with
/// ever larger cookies and never change them, so a listing resumed after one (`readdir` with
/// an offset) neither skips nor repeats entries when others are removed meanwhile.
#[derive(Clone, Debug)]
pub struct DirEntry {
    pub name: Vec<u8>,
    pub node: usize,
    pub cookie: u64,
}

#[derive(Clone, Debug)]
pub enum Kind {
    Dir {
        parent: usize,
        entries: Vec<DirEntry>,
    },
    File {
        size: u64,
        /// Bytes allocated, when the fixture sets it; otherwise the size in 4 KiB blocks.
        alloc: Option<u64>,
        data: Data,
    },
    Symlink(Vec<u8>),
    /// Removed; its number is not used again.
    Gone,
}

#[derive(Clone, Debug)]
pub struct Inode {
    pub kind: Kind,
    /// Seconds since the epoch.
    pub mtime: i64,
    /// Permission bits.
    pub mode: u32,
    pub uid: u32,
    pub gid: u32,
    /// Hard links to a file.
    pub nlink: u32,
    pub fault: Option<Fault>,
    /// How long a directory's listing takes to open.
    pub delay: Duration,
}

impl Kind {
    /// An empty directory; [`Tree::insert`] sets its parent.
    #[must_use]
    pub fn dir() -> Kind {
        Kind::Dir {
            parent: 0,
            entries: vec![],
        }
    }

    /// A file of the stored bytes `b`.
    #[must_use]
    pub fn stored(b: Vec<u8>) -> Kind {
        Kind::File {
            size: b.len() as u64,
            alloc: None,
            data: Data::Bytes(b),
        }
    }

    /// A file of `size` bytes generated from `seed`.
    #[must_use]
    pub fn seeded(size: u64, seed: u64) -> Kind {
        Kind::File {
            size,
            alloc: None,
            data: Data::Seeded { seed, len: size },
        }
    }
}

impl Inode {
    fn new(kind: Kind, mtime: i64, mode: u32, (uid, gid): (u32, u32)) -> Inode {
        Inode {
            kind,
            mtime,
            mode,
            uid,
            gid,
            nlink: 1,
            fault: None,
            delay: Duration::ZERO,
        }
    }

    #[must_use]
    pub fn is_dir(&self) -> bool {
        matches!(self.kind, Kind::Dir { .. })
    }
}

#[derive(Clone, Debug)]
pub struct Tree {
    pub nodes: Vec<Inode>,
    /// Owner (uid, gid) of the nodes [`Tree::insert`] makes.
    pub owner: (u32, u32),
    /// The cookie of the next directory entry; 1 and 2 are `.` and `..`.
    next_cookie: u64,
}

impl Tree {
    /// A tree with an empty root, owned by `owner` (uid, gid) as the nodes added to it are.
    #[must_use]
    pub fn new(root_mtime: i64, owner: (u32, u32)) -> Tree {
        Tree {
            nodes: vec![Inode::new(Kind::dir(), root_mtime, 0o755, owner)],
            owner,
            next_cookie: 3,
        }
    }

    /// Node `n`, if it exists and has not been removed.
    #[must_use]
    pub fn get(&self, n: usize) -> Option<&Inode> {
        self.nodes.get(n).filter(|i| !matches!(i.kind, Kind::Gone))
    }

    fn entries(&self, dir: usize) -> Result<&Vec<DirEntry>, FsError> {
        match self.get(dir).map(|i| &i.kind) {
            Some(Kind::Dir { entries, .. }) => Ok(entries),
            Some(_) => Err(FsError::NotDir),
            None => Err(FsError::NoEnt),
        }
    }

    fn entries_mut(&mut self, dir: usize) -> Result<&mut Vec<DirEntry>, FsError> {
        match self.nodes.get_mut(dir).map(|i| &mut i.kind) {
            Some(Kind::Dir { entries, .. }) => Ok(entries),
            Some(Kind::Gone) | None => Err(FsError::NoEnt),
            Some(_) => Err(FsError::NotDir),
        }
    }

    /// The entries of directory `dir`, in the order they were added (by cookie).
    ///
    /// # Errors
    ///
    /// `dir` is not a directory.
    pub fn list(&self, dir: usize) -> Result<&[DirEntry], FsError> {
        self.entries(dir).map(Vec::as_slice)
    }

    /// The parent of directory `dir` (the root's is itself).
    #[must_use]
    pub fn parent(&self, dir: usize) -> usize {
        match self.get(dir).map(|i| &i.kind) {
            Some(Kind::Dir { parent, .. }) => *parent,
            _ => 0,
        }
    }

    /// The node named `name` in directory `dir`.
    ///
    /// # Errors
    ///
    /// `dir` is not a directory, or has no such entry.
    pub fn lookup(&self, dir: usize, name: &[u8]) -> Result<usize, FsError> {
        self.entries(dir)?
            .iter()
            .find(|e| e.name == name)
            .map(|e| e.node)
            .ok_or(FsError::NoEnt)
    }

    /// The node at `path` below the root, `/`-separated.
    ///
    /// # Errors
    ///
    /// Some component does not exist or is not a directory.
    pub fn resolve(&self, path: &str) -> Result<usize, FsError> {
        path.split('/')
            .filter(|p| !p.is_empty())
            .try_fold(0, |d, p| self.lookup(d, p.as_bytes()))
    }

    /// Adds `kind` as `name` in `dir`.
    ///
    /// # Errors
    ///
    /// `dir` is not a directory, or already has `name`.
    pub fn insert(
        &mut self,
        dir: usize,
        name: &[u8],
        kind: Kind,
        mtime: i64,
        mode: u32,
    ) -> Result<usize, FsError> {
        if name.is_empty() || name == b"." || name == b".." || name.contains(&b'/') {
            return Err(FsError::Inval);
        }
        // Checked before the node is made, so a failure leaves nothing behind.
        if self.entries(dir)?.iter().any(|e| e.name == name) {
            return Err(FsError::Exist);
        }
        let n = self.nodes.len();
        let kind = match kind {
            Kind::Dir { entries, .. } => Kind::Dir {
                parent: dir,
                entries,
            },
            k => k,
        };
        self.nodes.push(Inode::new(kind, mtime, mode, self.owner));
        self.add_entry(dir, name, n)?;
        self.nodes[dir].mtime = self.nodes[dir].mtime.max(mtime);
        Ok(n)
    }

    /// Appends `name` for node `n` to `dir`, with the next cookie.
    fn add_entry(&mut self, dir: usize, name: &[u8], n: usize) -> Result<(), FsError> {
        let cookie = self.next_cookie;
        self.entries_mut(dir)?.push(DirEntry {
            name: name.to_vec(),
            node: n,
            cookie,
        });
        self.next_cookie += 1;
        Ok(())
    }

    /// Adds a hard link `name` in `dir` to the file `n`.
    ///
    /// # Errors
    ///
    /// `n` is a directory, `dir` is not, or `name` exists.
    pub fn link(&mut self, n: usize, dir: usize, name: &[u8]) -> Result<(), FsError> {
        if self.get(n).ok_or(FsError::NoEnt)?.is_dir() {
            return Err(FsError::IsDir);
        }
        if self.entries(dir)?.iter().any(|e| e.name == name) {
            return Err(FsError::Exist);
        }
        self.add_entry(dir, name, n)?;
        self.nodes[n].nlink += 1;
        Ok(())
    }

    /// Takes the entry `name` out of `dir`, returning the node it named.
    fn detach(&mut self, dir: usize, name: &[u8]) -> Result<usize, FsError> {
        let entries = self.entries_mut(dir)?;
        let i = entries
            .iter()
            .position(|e| e.name == name)
            .ok_or(FsError::NoEnt)?;
        Ok(entries.remove(i).node)
    }

    fn drop_link(&mut self, n: usize) {
        let node = &mut self.nodes[n];
        node.nlink = node.nlink.saturating_sub(1);
        if node.nlink == 0 || node.is_dir() {
            node.kind = Kind::Gone;
        }
    }

    /// Removes the file or symlink `name` from `dir`, or with `want_dir` the empty directory.
    fn remove(&mut self, dir: usize, name: &[u8], want_dir: bool, t: i64) -> Result<(), FsError> {
        let n = self.lookup(dir, name)?;
        match (&self.nodes[n].kind, want_dir) {
            (Kind::Dir { entries, .. }, true) if !entries.is_empty() => {
                return Err(FsError::NotEmpty);
            }
            (Kind::Dir { .. }, false) => return Err(FsError::IsDir),
            (k, true) if !matches!(k, Kind::Dir { .. }) => return Err(FsError::NotDir),
            _ => {}
        }
        self.detach(dir, name)?;
        self.drop_link(n);
        self.nodes[dir].mtime = t;
        Ok(())
    }

    /// `unlink(2)`.
    ///
    /// # Errors
    ///
    /// No such entry, or it is a directory.
    pub fn unlink(&mut self, dir: usize, name: &[u8], t: i64) -> Result<(), FsError> {
        self.remove(dir, name, false, t)
    }

    /// `rmdir(2)`.
    ///
    /// # Errors
    ///
    /// No such entry, it is not a directory, or not empty.
    pub fn rmdir(&mut self, dir: usize, name: &[u8], t: i64) -> Result<(), FsError> {
        self.remove(dir, name, true, t)
    }

    /// Whether `n` is `anc` or below it.
    fn below(&self, mut n: usize, anc: usize) -> bool {
        loop {
            if n == anc {
                return true;
            }
            if n == 0 {
                return false;
            }
            n = self.parent(n);
        }
    }

    /// `renameat2(2)` with `RENAME_NOREPLACE` (`noreplace`) or `RENAME_EXCHANGE` (`exchange`).
    ///
    /// # Errors
    ///
    /// As `rename(2)`: no source, the target exists with `noreplace` (or is missing with
    /// `exchange`), a directory moved below itself, or a file and a directory swapped for one
    /// another.
    #[expect(clippy::too_many_arguments, reason = "the arguments of renameat2")]
    pub fn rename(
        &mut self,
        dir: usize,
        name: &[u8],
        to_dir: usize,
        to_name: &[u8],
        noreplace: bool,
        exchange: bool,
        t: i64,
    ) -> Result<(), FsError> {
        let n = self.lookup(dir, name)?;
        self.entries(to_dir)?;
        let existing = self.lookup(to_dir, to_name).ok();
        if self.nodes[n].is_dir() && self.below(to_dir, n) {
            return Err(FsError::Inval);
        }
        if exchange {
            let m = existing.ok_or(FsError::NoEnt)?;
            if self.nodes[m].is_dir() && self.below(dir, m) {
                return Err(FsError::Inval);
            }
            self.set_entry(dir, name, m);
            self.set_entry(to_dir, to_name, n);
            self.reparent(m, dir);
            self.reparent(n, to_dir);
            self.nodes[dir].mtime = t;
            self.nodes[to_dir].mtime = t;
            return Ok(());
        }
        if existing == Some(n) {
            return Ok(());
        }
        if let Some(m) = existing {
            if noreplace {
                return Err(FsError::Exist);
            }
            match (self.nodes[n].is_dir(), &self.nodes[m].kind) {
                (true, Kind::Dir { entries, .. }) if !entries.is_empty() => {
                    return Err(FsError::NotEmpty);
                }
                (true, Kind::Dir { .. }) | (false, Kind::File { .. } | Kind::Symlink(_)) => {}
                (true, _) => return Err(FsError::NotDir),
                (false, _) => return Err(FsError::IsDir),
            }
            self.detach(to_dir, to_name)?;
            self.drop_link(m);
        }
        self.detach(dir, name)?;
        self.add_entry(to_dir, to_name, n)?;
        self.reparent(n, to_dir);
        self.nodes[dir].mtime = t;
        self.nodes[to_dir].mtime = t;
        Ok(())
    }

    fn set_entry(&mut self, dir: usize, name: &[u8], n: usize) {
        if let Ok(entries) = self.entries_mut(dir)
            && let Some(e) = entries.iter_mut().find(|e| e.name == name)
        {
            e.node = n;
        }
    }

    fn reparent(&mut self, n: usize, to: usize) {
        if let Kind::Dir { parent, .. } = &mut self.nodes[n].kind {
            *parent = to;
        }
    }

    /// Length of `n` as `stat` reports it.
    #[must_use]
    pub fn size(&self, n: usize) -> u64 {
        match self.get(n).map(|i| &i.kind) {
            Some(Kind::File { size, .. }) => *size,
            Some(Kind::Symlink(t)) => t.len() as u64,
            Some(Kind::Dir { .. }) => DIR_SIZE,
            _ => 0,
        }
    }

    /// Bytes allocated to `n`.
    #[must_use]
    pub fn alloc(&self, n: usize) -> u64 {
        match self.get(n).map(|i| &i.kind) {
            Some(Kind::File { size, alloc, .. }) => alloc.unwrap_or(size.div_ceil(4096) * 4096),
            Some(Kind::Dir { .. }) => DIR_SIZE,
            _ => 0,
        }
    }

    /// Bytes allocated to every node, each counted once however many links it has.
    #[must_use]
    pub fn used(&self) -> u64 {
        (0..self.nodes.len()).map(|n| self.alloc(n)).sum()
    }

    /// Length of every node below the root added up, each counted once however many links
    /// it has.
    #[must_use]
    pub fn apparent(&self) -> u64 {
        (1..self.nodes.len()).map(|n| self.size(n)).sum()
    }

    /// Nodes that exist.
    #[must_use]
    pub fn live(&self) -> usize {
        self.nodes
            .iter()
            .filter(|i| !matches!(i.kind, Kind::Gone))
            .count()
    }

    /// Subdirectories of `dir`, for its link count.
    #[must_use]
    pub fn subdirs(&self, dir: usize) -> usize {
        self.entries(dir).map_or(0, |e| {
            e.iter().filter(|e| self.nodes[e.node].is_dir()).count()
        })
    }
}

#[cfg(test)]
mod tests;
