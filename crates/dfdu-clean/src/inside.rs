//! What the scan saw inside a directory item, compared with the disk just before the item is
//! deleted. The item's own identity says nothing about its contents: a file added or replaced
//! deep inside changes only its own directory, and modification times have whole-second
//! resolution here, so a change in the second of the scan would not show. Each directory the
//! scan read completely is compared by its entries instead: their names, types and inode
//! numbers. (A freed inode number is soon reused, so a directory replaced by a symlink can
//! have its number; the type tells them apart. A file deleted and another written under its
//! name in the same second, reusing its number, is not seen.)
//! Directories the scan did not read to the end (a delete of a directory still being scanned,
//! a read error) are not compared: the confirmation said their size was a lower bound.

use std::collections::HashMap;
use std::ffi::OsStr;
use std::fs;
use std::path::{Path, PathBuf};

use dfdu_tree::{Kind, name_bytes};

/// A directory's entries as the scan saw them.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Listing {
    pub ino: u64,
    /// `(name_hash(name), inode, type)` of each entry, sorted; inode 0 where the scan did not
    /// get one, which then matches any.
    pub entries: Vec<(u64, u64, Kind)>,
}

/// Listings of the directories in an item, by path relative to the item ("" for the item).
pub type Listings = HashMap<PathBuf, Listing>;

/// FNV-1a over a name's bytes.
pub fn name_hash(name: &[u8]) -> u64 {
    name.iter().fold(0xcbf2_9ce4_8422_2325, |h, &b| {
        (h ^ b as u64).wrapping_mul(0x100_0000_01b3)
    })
}

/// Compares the directories under `root` (the item) with `expect`. Returns the first that
/// differs, relative to the item, with what changed.
pub fn compare(root: &Path, expect: &Listings) -> Result<(), String> {
    walk(root, Path::new(""), expect)
}

fn walk(dir: &Path, rel: &Path, expect: &Listings) -> Result<(), String> {
    let Some(want) = expect.get(rel) else {
        return Ok(());
    };
    let shown = || {
        if rel.as_os_str().is_empty() {
            "its contents".to_string()
        } else {
            rel.display().to_string()
        }
    };
    let mut got = vec![];
    let mut subdirs = vec![];
    let rd = fs::read_dir(dir).map_err(|e| format!("{}: {e}", shown()))?;
    for e in rd {
        let e = e.map_err(|e| format!("{}: {e}", shown()))?;
        let name = e.file_name();
        let ft = e.file_type().map_err(|e| format!("{}: {e}", shown()))?;
        got.push((name_hash(&name_bytes(&name)), entry_ino(&e), kind_of(ft)));
        if ft.is_dir() {
            subdirs.push(name);
        }
    }
    got.sort_unstable();
    if !same(&want.entries, &got) {
        // The inode a directory entry carries can differ from the file's own on some
        // filesystems (overlayfs): compare the files' own before calling it changed.
        let own: Option<Vec<(u64, u64, Kind)>> = fs::read_dir(dir).ok().and_then(|rd| {
            let mut v: Vec<(u64, u64, Kind)> = rd
                .map(|e| {
                    let e = e.ok()?;
                    let m = fs::symlink_metadata(e.path()).ok()?;
                    let name = name_hash(&name_bytes(&e.file_name()));
                    Some((name, meta_ino(&m), kind_of(m.file_type())))
                })
                .collect::<Option<_>>()?;
            v.sort_unstable();
            Some(v)
        });
        if !own.is_some_and(|v| same(&want.entries, &v)) {
            return Err(format!("{} changed since the scan", shown()));
        }
    }
    for name in subdirs {
        walk(&dir.join(&name), &rel.join(&name), expect)?;
    }
    Ok(())
}

/// Entries match by name and type, and by inode where both are known.
fn same(want: &[(u64, u64, Kind)], got: &[(u64, u64, Kind)]) -> bool {
    want.len() == got.len()
        && want
            .iter()
            .zip(got)
            .all(|(w, g)| w.0 == g.0 && w.2 == g.2 && (w.1 == 0 || g.1 == 0 || w.1 == g.1))
}

fn kind_of(ft: fs::FileType) -> Kind {
    if ft.is_symlink() {
        Kind::Symlink
    } else if ft.is_dir() {
        Kind::Dir
    } else if ft.is_file() {
        Kind::File
    } else {
        Kind::Other
    }
}

#[cfg(unix)]
fn entry_ino(e: &fs::DirEntry) -> u64 {
    std::os::unix::fs::DirEntryExt::ino(e)
}

#[cfg(unix)]
fn meta_ino(m: &fs::Metadata) -> u64 {
    std::os::unix::fs::MetadataExt::ino(m)
}

/// Windows: std reports no file ids, so names alone are compared.
#[cfg(not(unix))]
fn entry_ino(_e: &fs::DirEntry) -> u64 {
    0
}

#[cfg(not(unix))]
fn meta_ino(_m: &fs::Metadata) -> u64 {
    0
}

/// The listing of `dir` as it is now, for tests and for callers without a tree.
pub fn listing_of(dir: &Path) -> std::io::Result<Listing> {
    let mut entries = vec![];
    for e in fs::read_dir(dir)? {
        let e = e?;
        let name = name_hash(&name_bytes(OsStr::new(&e.file_name())));
        entries.push((name, entry_ino(&e), kind_of(e.file_type()?)));
    }
    entries.sort_unstable();
    Ok(Listing {
        ino: meta_ino(&fs::symlink_metadata(dir)?),
        entries,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn listings(root: &Path) -> Listings {
        let mut out = Listings::new();
        let mut stack = vec![PathBuf::new()];
        while let Some(rel) = stack.pop() {
            let dir = root.join(&rel);
            out.insert(rel.clone(), listing_of(&dir).unwrap());
            for e in fs::read_dir(&dir).unwrap().flatten() {
                if e.file_type().unwrap().is_dir() {
                    stack.push(rel.join(e.file_name()));
                }
            }
        }
        out
    }

    #[test]
    fn changes_deep_inside_are_found() {
        let tmp = tempfile::tempdir().unwrap();
        let d = tmp.path().join("d");
        fs::create_dir_all(d.join("a/b")).unwrap();
        fs::write(d.join("a/b/f"), b"x").unwrap();
        fs::write(d.join("g"), b"x").unwrap();
        let seen = listings(&d);
        compare(&d, &seen).unwrap();

        // A new file, in the same second as the scan.
        fs::write(d.join("a/b/new"), b"x").unwrap();
        let e = compare(&d, &seen).unwrap_err();
        assert!(e.contains("a/b") || e.contains("a\\b"), "{e}");
        fs::remove_file(d.join("a/b/new")).unwrap();
        compare(&d, &seen).unwrap();

        // A file replaced by another of the same name.
        fs::write(d.join("a/b/f2"), b"y").unwrap();
        fs::rename(d.join("a/b/f2"), d.join("a/b/f")).unwrap();
        #[cfg(unix)]
        assert!(compare(&d, &seen).is_err());

        // Not compared where the scan has no listing.
        let mut partial = listings(&d);
        partial.remove(Path::new("a/b"));
        fs::write(d.join("a/b/new"), b"x").unwrap();
        compare(&d, &partial).unwrap();
    }
}
