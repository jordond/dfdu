//! Building a fixture's tree: its imports, then its entries in order, each with the
//! directories its path runs through.

use anyhow::{Context as _, anyhow, bail};

use super::import::{expand, import};
use super::{Amount, Entry, EntryKind, FaultName, Fixture, parse_age, parse_duration};
use crate::Error;
use crate::fill;
use crate::tree::{Data, Fault, Kind, Tree, seed_of};

impl Fixture {
    /// The tree the fixture describes.
    ///
    /// # Errors
    ///
    /// An entry is malformed, its path runs through a file, it is given twice, or it links to
    /// a path that is not an earlier file; an import cannot be read.
    pub fn build(&self) -> Result<Tree, Error> {
        let mut tree = Tree::new(0, (self.uid, self.gid));
        self.build_at(&mut tree, "")?;
        Ok(tree)
    }

    /// Builds the fixture into `tree` with its root at the directory `at` (made with its
    /// parents when missing), owned by the fixture's uid and gid. Imports come first, then the
    /// entries.
    ///
    /// # Errors
    ///
    /// As [`Fixture::build`], or `at` runs through a file.
    pub fn build_at(&self, tree: &mut Tree, at: &str) -> Result<(), Error> {
        Ok(self.build_into(tree, at)?)
    }

    fn build_into(&self, tree: &mut Tree, at: &str) -> anyhow::Result<()> {
        let now = self.now()?;
        let default_mtime = now - parse_age(&self.age)?;
        tree.owner = (self.uid, self.gid);
        let root = mkdir_p(tree, 0, at, default_mtime)?;
        let node = &mut tree.nodes[root];
        node.mtime = default_mtime;
        (node.uid, node.gid) = tree.owner;
        let under = |p: &str| {
            let p = p.trim_start_matches('/');
            if at.is_empty() {
                p.to_owned()
            } else {
                format!("{at}/{p}")
            }
        };
        for pattern in &self.import {
            for from in expand(pattern).with_context(|| format!("import {pattern:?}"))? {
                let dir = mkdir_p(tree, 0, &parent_of(&under(&from)), default_mtime)?;
                import(tree, dir, std::path::Path::new(&from), default_mtime)
                    .with_context(|| format!("import {from:?}"))?;
            }
        }
        for e in &self.entries {
            add(tree, e, &under, now, default_mtime)
                .with_context(|| format!("entry {:?}", e.path))?;
        }
        Ok(())
    }
}

fn parent_of(path: &str) -> String {
    path.rsplit_once('/')
        .map_or(String::new(), |(p, _)| p.to_owned())
}

/// Adds entry `e` at `under(e.path)`, its age counted back from `now`.
fn add(
    tree: &mut Tree,
    e: &Entry,
    under: &impl Fn(&str) -> String,
    now: i64,
    default_mtime: i64,
) -> anyhow::Result<()> {
    let mtime = e
        .age
        .as_deref()
        .map_or(Ok(default_mtime), |a| parse_age(a).map(|a| now - a))?;
    let path = under(&e.path);
    let (parent, name) = path.rsplit_once('/').unwrap_or(("", &path));
    let dir = mkdir_p(tree, 0, parent, mtime)?;
    if let Some(to) = &e.link {
        let target = tree
            .resolve(&under(to))
            .map_err(|_| anyhow!("link {to:?}: no such entry"))?;
        return tree
            .link(target, dir, name.as_bytes())
            .map_err(|err| anyhow!("link {to:?}: {err:?}"));
    }
    let kind = e.kind.unwrap_or(if e.fill.is_some() {
        EntryKind::Dir
    } else if e.target.is_some() {
        EntryKind::Symlink
    } else {
        EntryKind::File
    });
    let n = match kind {
        EntryKind::Dir => match tree.lookup(dir, name.as_bytes()) {
            // A directory made for an earlier entry's path: this entry sets its attributes.
            Ok(n) if tree.nodes[n].is_dir() => n,
            _ => insert(tree, dir, name, Kind::dir(), mtime, 0o755)?,
        },
        EntryKind::Symlink => {
            let target = e.target.as_deref().context("a symlink needs a target")?;
            let kind = Kind::Symlink(target.as_bytes().to_vec());
            insert(tree, dir, name, kind, mtime, 0o777)?
        }
        EntryKind::File => {
            let (size, data) = match (&e.text, &e.size) {
                (Some(t), _) => (t.len() as u64, Data::Bytes(t.as_bytes().to_vec())),
                (None, s) => {
                    let id = e.content.as_deref().unwrap_or(&e.path);
                    let size = s.as_ref().map_or(Ok(0), Amount::bytes)?;
                    let seed = seed_of(id.as_bytes());
                    (size, Data::Seeded { seed, len: size })
                }
            };
            let alloc = e.alloc.as_ref().map(Amount::bytes).transpose()?;
            insert(
                tree,
                dir,
                name,
                Kind::File { size, alloc, data },
                mtime,
                0o644,
            )?
        }
    };
    let node = &mut tree.nodes[n];
    node.mtime = mtime;
    if let Some(m) = e.mode {
        node.mode = m;
    }
    node.fault = e.error.as_ref().map(|f| match f {
        FaultName::Denied => Fault::Denied,
        FaultName::Io => Fault::Io,
    });
    if let Some(d) = &e.delay {
        node.delay = parse_duration(d)?;
    }
    if let Some(f) = &e.fill {
        fill::fill(tree, n, f, mtime, &e.path)?;
    }
    Ok(())
}

/// [`Tree::insert`], its error as a message.
pub(crate) fn insert(
    tree: &mut Tree,
    dir: usize,
    name: &str,
    kind: Kind,
    mtime: i64,
    mode: u32,
) -> anyhow::Result<usize> {
    tree.insert(dir, name.as_bytes(), kind, mtime, mode)
        .map_err(|e| anyhow!("{e:?}"))
}

/// The directory at `path` below `dir`, made with its parents when missing.
pub(crate) fn mkdir_p(
    tree: &mut Tree,
    dir: usize,
    path: &str,
    mtime: i64,
) -> anyhow::Result<usize> {
    let mut d = dir;
    for part in path.split('/').filter(|p| !p.is_empty()) {
        d = match tree.lookup(d, part.as_bytes()) {
            Ok(n) if tree.nodes[n].is_dir() => n,
            Ok(_) => bail!("{part:?} in {path:?} is not a directory"),
            Err(_) => insert(tree, d, part, Kind::dir(), mtime, 0o755)?,
        };
    }
    Ok(d)
}

#[cfg(test)]
mod tests;
