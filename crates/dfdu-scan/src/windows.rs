//! Windows backend: `GetFileInformationByHandleEx` with `FileIdExtdDirectoryInfo` into a 64 KiB
//! buffer per directory, which returns each entry's sizes, attributes, reparse tag, write time
//! and file id with its name, so no entry is opened to learn its size.
//!
//! Reading a block ([`DirStream::next_raw`]) takes the entries from that buffer, metadata
//! included. Stat'ing it ([`RawBlock::stat`]) converts the names and asks for the one thing the
//! enumeration lacks, the link count, for regular files on volumes that have hard links: one
//! `NtQueryInformationByName` per file relative to the directory handle, which the raw blocks
//! share, so chunk tasks spread these calls over the workers as `statx` is on Linux. A file
//! whose count is above one takes its sizes from that call too, as NTFS updates the directory
//! entry only of the name a file was changed through. The file id and the volume serial number
//! identify the file, so hard links are counted once as on Unix. Directories take the same call
//! for their allocation, which the enumeration reports as 0; symlinks and junctions need none.
//!
//! Reparse points whose tag is a name surrogate (symlinks, junctions, volume mount points) are
//! links: listed with their own size and never entered, so a scan stays on its volume except
//! for a scan root given as a link, which is followed. Cloud placeholders and offline files
//! (`WinStat::recalls_on_open`) are never opened: their sizes are the enumeration's (0 on disk
//! for OneDrive's online-only files), and a placeholder directory not yet listed is not
//! entered. A directory on another volume is a mount root (its `dev`, the volume serial
//! number, differs), as on other systems.
//!
//! Directories are opened by `\\?\` path (`dfdu_sys::verbatim_path`), so paths of any length
//! up to the 32,767 characters Windows allows are read, and names ending in a dot or a space
//! are kept as they are.

use std::collections::HashSet;
use std::ffi::OsString;
use std::io;
use std::os::windows::ffi::OsStringExt;
use std::os::windows::io::{AsHandle, OwnedHandle};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use dfdu_sys::{DirReader, LinkQuery, Volume};
use dfdu_tree::{ErrKind, Kind, Meta, Subvol, WinStat};

use crate::FsClass;
use crate::backend::{Backend, Block, DirStream, PortableBackend, Raw, RawBlock};

#[derive(Default, Debug)]
pub struct WindowsBackend {
    portable: PortableBackend,
    /// Shared by every directory: records whether `NtQueryInformationByName` works.
    links: Arc<LinkQuery>,
}

/// A directory to read: its verbatim path, and whether a link at that path is followed (only
/// for a scan root).
#[derive(Clone, Debug)]
pub struct WinLoc {
    path: PathBuf,
    follow: bool,
}

impl WinLoc {
    pub fn path(&self) -> &Path {
        &self.path
    }
}

/// An open directory, shared by its stream and its raw blocks.
struct Dir {
    handle: OwnedHandle,
    path: PathBuf,
    volume: Volume,
    links: Arc<LinkQuery>,
}

pub struct WindowsStream {
    dir: Arc<Dir>,
    reader: DirReader,
    pending_err: Option<io::Error>,
    done: bool,
}

/// Entries read from the enumeration buffer: UTF-16 names in one buffer, and each entry's
/// name span and metadata.
struct WinRaw {
    dir: Arc<Dir>,
    names: Vec<u16>,
    entries: Vec<(u32, u32, WinStat)>,
}

impl RawBlock<WinLoc> for WinRaw {
    fn len(&self) -> usize {
        self.entries.len()
    }

    fn stat(&mut self, out: &mut Block<WinLoc>) -> io::Result<()> {
        let mut name8 = Vec::new();
        for &(off, len, mut st) in &self.entries {
            let name = &self.names[off as usize..(off + len) as usize];
            name8.clear();
            dfdu_tree::name_bytes_wide(name, &mut name8);
            st.volume = self.dir.volume.serial;
            let kind = st.kind();
            let mut meta = Ok(Meta::from_windows(&st));
            if kind == Kind::File && self.dir.volume.hard_links && !st.recalls_on_open() {
                let reparse = st.attributes & dfdu_tree::win_attr::REPARSE_POINT != 0;
                match self
                    .dir
                    .links
                    .query(self.dir.handle.as_handle(), name, reparse)
                {
                    Ok(l) => {
                        st.nlink = l.nlink;
                        st.size = l.size;
                        st.alloc = l.alloc;
                        meta = Ok(Meta::from_windows(&st));
                    }
                    // Listed but not readable: counted from the listing, as a single link.
                    Err(e) if e.kind() == io::ErrorKind::PermissionDenied => {}
                    Err(e) => meta = Err(ErrKind::from(&e)),
                }
            } else if kind == Kind::Dir && !st.recalls_on_open() {
                // The listing reports 0 for a directory; its index takes clusters once it no
                // longer fits in the MFT record. Unreadable: counted as listed.
                let reparse = st.attributes & dfdu_tree::win_attr::REPARSE_POINT != 0;
                if let Ok(l) = self
                    .dir
                    .links
                    .query(self.dir.handle.as_handle(), name, reparse)
                {
                    st.alloc = l.alloc;
                    meta = Ok(Meta::from_windows(&st));
                }
            }
            let child = match &meta {
                Ok(m) if m.kind == Kind::Dir && !st.recalls_on_open() => Some(WinLoc {
                    path: self.dir.path.join(OsString::from_wide(name)),
                    follow: false,
                }),
                _ => None,
            };
            out.push(&name8, meta, child);
        }
        self.entries.clear();
        Ok(())
    }
}

impl DirStream for WindowsStream {
    type Loc = WinLoc;

    fn next_raw(
        &mut self,
        max_entries: usize,
        max_name_bytes: usize,
    ) -> io::Result<(Raw<WinLoc>, bool)> {
        if let Some(e) = self.pending_err.take() {
            self.done = true;
            return Err(e);
        }
        let mut raw = WinRaw {
            dir: self.dir.clone(),
            names: Vec::new(),
            entries: Vec::new(),
        };
        while !self.done && raw.entries.len() < max_entries && raw.names.len() * 2 < max_name_bytes
        {
            match self.reader.next(self.dir.handle.as_handle()) {
                Ok(None) => self.done = true,
                Err(e) => {
                    if raw.entries.is_empty() {
                        self.done = true;
                        return Err(e);
                    }
                    self.pending_err = Some(e);
                    break;
                }
                Ok(Some(e)) => {
                    let off = raw.names.len() as u32;
                    raw.names.extend(e.name_units());
                    let len = raw.names.len() as u32 - off;
                    raw.entries.push((off, len, e.stat));
                }
            }
        }
        Ok((Box::new(raw), !self.done))
    }
}

impl Backend for WindowsBackend {
    type Loc = WinLoc;
    type Stream = WindowsStream;

    fn read_prefix(&self, dir: &Self::Loc, name: &[u8], len: usize) -> Option<Vec<u8>> {
        crate::backend::read_prefix_at(dir.path(), name, len)
    }

    fn loc_from_path(&self, path: &Path) -> WinLoc {
        WinLoc {
            path: dfdu_sys::verbatim_path(path).unwrap_or_else(|_| path.to_path_buf()),
            follow: true,
        }
    }

    fn stat_root(&self, loc: &WinLoc) -> io::Result<Meta> {
        Ok(Meta::from_windows(&dfdu_sys::stat_path(&loc.path, true)?))
    }

    fn open(&self, loc: &WinLoc, _storage: dfdu_sys::Storage) -> io::Result<WindowsStream> {
        let handle = dfdu_sys::open_dir(&loc.path, loc.follow)?;
        let volume = dfdu_sys::volume_of(handle.as_handle())?;
        Ok(WindowsStream {
            dir: Arc::new(Dir {
                handle,
                path: loc.path.clone(),
                volume,
                links: self.links.clone(),
            }),
            reader: DirReader::default(),
            pending_err: None,
            done: false,
        })
    }

    fn subvolume(&self, loc: &WinLoc) -> io::Result<Option<Subvol>> {
        self.portable.subvolume(&loc.path)
    }

    fn shares_extents(&self, loc: &WinLoc) -> bool {
        self.portable.shares_extents(&loc.path)
    }

    fn data_profile(&self, loc: &WinLoc) -> Option<(&'static str, f64)> {
        self.portable.data_profile(&loc.path)
    }

    fn duplicate_mounts(&self, scope: &Path) -> Option<HashSet<u64>> {
        self.portable.duplicate_mounts(scope)
    }

    fn mount_class(&self, mount: u64, dev: u64) -> Option<FsClass> {
        self.portable.mount_class(mount, dev)
    }

    fn mount_storage(&self, mount: u64, dev: u64) -> dfdu_sys::Storage {
        self.portable.mount_storage(mount, dev)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::os::windows::ffi::OsStrExt;

    /// Every entry of a directory as `(name, meta)`, sorted by name.
    fn list<B: Backend>(b: &B, dir: &Path) -> Vec<(Vec<u8>, Result<Meta, ErrKind>)> {
        let mut s = b
            .open(&b.loc_from_path(dir), dfdu_sys::Storage::Unknown)
            .unwrap();
        let mut block = Block::default();
        // A small block size, so entries are read across several calls.
        loop {
            let (mut raw, more) = s.next_raw(3, 1 << 20).unwrap();
            raw.stat(&mut block).unwrap();
            if !more {
                break;
            }
        }
        let mut out: Vec<_> = block
            .entries
            .iter()
            .map(|e| (block.name(e.name).to_vec(), e.meta))
            .collect();
        out.sort_by(|a, b| a.0.cmp(&b.0));
        out
    }

    /// A junction (no privilege needed, unlike symlinks); `false` if `mklink` failed.
    fn junction(link: &Path, target: &Path) -> bool {
        std::process::Command::new("cmd")
            .arg("/C")
            .arg("mklink")
            .arg("/J")
            .arg(link)
            .arg(target)
            .output()
            .is_ok_and(|o| o.status.success())
    }

    #[test]
    fn entries_match_the_portable_backend() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        fs::create_dir(root.join("sub")).unwrap();
        for i in 0..20 {
            fs::write(root.join(format!("f{i}")), vec![1u8; i * 1000]).unwrap();
        }
        fs::hard_link(root.join("f3"), root.join("sub").join("link")).unwrap();
        // Symlinks need Developer Mode or an elevated process (CI runners are elevated).
        let symlinks = std::os::windows::fs::symlink_dir("sub", root.join("sym")).is_ok()
            && std::os::windows::fs::symlink_file("nowhere", root.join("dangling")).is_ok();
        assert!(junction(&root.join("junction"), &root.join("sub")));
        // A name with an unpaired surrogate, which is not valid UTF-16.
        let bad: Vec<u16> = "bad-".encode_utf16().chain([0xD800]).collect();
        fs::write(root.join(OsString::from_wide(&bad)), b"x").unwrap();
        let ours = list(&WindowsBackend::default(), root);
        let theirs = list(&PortableBackend::default(), root);
        assert_eq!(ours.len(), if symlinks { 25 } else { 23 });
        assert_eq!(ours, theirs);
        let by_name = |n: &str| &ours.iter().find(|e| e.0 == n.as_bytes()).unwrap().1;
        assert_eq!(by_name("f3").unwrap().nlink, 2);
        assert_eq!(by_name("f4").unwrap().nlink, 1);
        assert_eq!(by_name("junction").unwrap().kind, Kind::Symlink);
        assert_eq!(by_name("sub").unwrap().kind, Kind::Dir);
        // File ids and the volume serial identify a file: the two names of f3 agree.
        let sub = list(&WindowsBackend::default(), &root.join("sub"));
        let (f3, link) = (by_name("f3").unwrap(), sub[0].1.unwrap());
        assert_eq!((f3.dev, f3.ino), (link.dev, link.ino));
        assert_ne!(f3.ino, by_name("f4").unwrap().ino);
        let (w, p) = (WindowsBackend::default(), PortableBackend::default());
        assert_eq!(
            w.stat_root(&w.loc_from_path(root)).unwrap(),
            p.stat_root(&root.to_path_buf()).unwrap()
        );
        // A junction given as the root is followed.
        let j = root.join("junction");
        let jm = w.stat_root(&w.loc_from_path(&j)).unwrap();
        assert_eq!(jm.kind, Kind::Dir);
        assert_eq!(list(&w, &j), sub);
    }

    #[test]
    fn reads_paths_longer_than_max_path() {
        let tmp = tempfile::tempdir().unwrap();
        let mut deep = tmp.path().to_path_buf();
        // std adds the `\\?\` prefix to long paths itself.
        for i in 0..30 {
            deep.push(format!("{i:03}-{}", "y".repeat(200)));
            fs::create_dir(&deep).unwrap();
        }
        fs::write(deep.join("leaf"), [7u8; 99]).unwrap();
        // A name Win32 path parsing would strip the dot from.
        let dotted = dfdu_sys::verbatim_path(&deep).unwrap().join("dot.");
        fs::create_dir(&dotted).unwrap();
        fs::write(dotted.join("in"), [1u8; 5]).unwrap();
        assert!(deep.as_os_str().encode_wide().count() > 6000);
        let b = WindowsBackend::default();
        let entries = list(&b, &deep);
        assert_eq!(entries.len(), 2, "{entries:?}");
        assert_eq!(entries[1].0, b"leaf");
        assert_eq!(entries[1].1.unwrap().apparent, 99);
        assert_eq!(entries[0].1.unwrap().kind, Kind::Dir);
        let mut s = b
            .open(&b.loc_from_path(&deep), dfdu_sys::Storage::Unknown)
            .unwrap();
        let mut block = Block::default();
        let (mut raw, _) = s.next_raw(10, 1 << 20).unwrap();
        raw.stat(&mut block).unwrap();
        let child = block
            .entries
            .iter()
            .find_map(|e| e.child.clone())
            .expect("dot. is a directory");
        let inner = b
            .open(&child, dfdu_sys::Storage::Unknown)
            .unwrap()
            .next_raw(10, 1 << 20)
            .unwrap()
            .0
            .len();
        assert_eq!(inner, 1);
        assert_eq!(
            b.stat_root(&b.loc_from_path(&deep)).unwrap().kind,
            Kind::Dir
        );
        drop(s);
        let _ = fs::remove_dir_all(&dotted);
    }
}
