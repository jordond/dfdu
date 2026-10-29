//! What the Linux and macOS backends share: opening a directory by a path longer than the
//! kernel accepts, and the buffer of NUL-terminated names a raw block keeps.

use std::ffi::{CStr, OsStr};
use std::io;
use std::os::fd::{AsFd, OwnedFd};
use std::path::{Component, Path, PathBuf};

use rustix::fs::{CWD, Mode, OFlags};

/// Paths are opened in pieces of at most this many bytes when the whole is too long.
#[cfg(target_os = "linux")]
const PIECE: usize = 2048;
#[cfg(target_os = "macos")]
const PIECE: usize = 768;

/// Opens the directory at `path`, in pieces when the path is longer than the kernel accepts.
/// `last` is added to the flags of the open that reaches the directory itself (`NOFOLLOW` not
/// to follow a symlink there); the leading pieces are followed as the path says.
pub(crate) fn open_dir(path: &Path, last: OFlags) -> io::Result<OwnedFd> {
    let flags = OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC;
    match rustix::fs::open(path, flags | last, Mode::empty()) {
        Err(rustix::io::Errno::NAMETOOLONG) => {}
        r => return r.map_err(Into::into),
    }
    let mut fd: Option<OwnedFd> = None;
    let mut piece = PathBuf::new();
    for c in path.components() {
        let part: &OsStr = match c {
            Component::RootDir => OsStr::new("/"),
            Component::CurDir => continue,
            Component::ParentDir => OsStr::new(".."),
            Component::Normal(n) => n,
            Component::Prefix(_) => unreachable!("no path prefixes on Unix"),
        };
        if !piece.as_os_str().is_empty() && piece.as_os_str().len() + part.len() + 1 > PIECE {
            let base = fd.as_ref().map_or(CWD, |f| f.as_fd());
            fd = Some(rustix::fs::openat(base, &piece, flags, Mode::empty())?);
            piece.clear();
        }
        piece.push(part);
    }
    let base = fd.as_ref().map_or(CWD, |f| f.as_fd());
    Ok(rustix::fs::openat(
        base,
        &piece,
        flags | last,
        Mode::empty(),
    )?)
}

/// Appends `name` and its NUL to `names`; returns where it starts.
#[expect(
    clippy::cast_possible_truncation,
    reason = "a block holds at most `Limits::block_name_bytes` of names, far below `u32::MAX`"
)]
pub(crate) fn push_name(names: &mut Vec<u8>, name: &[u8]) -> u32 {
    let off = names.len() as u32;
    names.extend_from_slice(name);
    names.push(0);
    off
}

/// The name [`push_name`] put at `off` in `names`.
#[expect(
    clippy::expect_used,
    reason = "`push_name` ends every name with one NUL, and only its offsets are passed here"
)]
pub(crate) fn name_at(names: &[u8], off: u32) -> &CStr {
    let rest = &names[off as usize..];
    let end = rest.iter().position(|&b| b == 0).expect("NUL-terminated");
    CStr::from_bytes_with_nul(&rest[..=end]).expect("one NUL")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::Backend;
    use crate::backend::tests::list;
    use dfdu_tree::Kind;

    #[test]
    fn names_come_back_from_their_offsets() {
        let mut names = Vec::new();
        let a = push_name(&mut names, b"first");
        let b = push_name(&mut names, b"");
        let c = push_name(&mut names, b"\xffthird");
        assert_eq!(name_at(&names, a).to_bytes(), b"first");
        assert_eq!(name_at(&names, b).to_bytes(), b"");
        assert_eq!(name_at(&names, c).to_bytes(), b"\xffthird");
    }

    #[test]
    fn opens_paths_longer_than_path_max() {
        let tmp = tempfile::tempdir().unwrap();
        let mut deep = tmp.path().to_path_buf();
        // Created one level at a time through descriptors, as no single path call reaches it.
        let mut fd = open_dir(&deep, OFlags::empty()).unwrap();
        for i in 0..30 {
            let name = format!("{i:03}-{}", "y".repeat(200));
            rustix::fs::mkdirat(&fd, name.as_str(), Mode::from_raw_mode(0o755)).unwrap();
            fd = rustix::fs::openat(&fd, name.as_str(), OFlags::DIRECTORY, Mode::empty()).unwrap();
            deep.push(name);
        }
        let f = rustix::fs::openat(
            &fd,
            "leaf",
            OFlags::CREATE | OFlags::WRONLY,
            Mode::from_raw_mode(0o644),
        )
        .unwrap();
        rustix::io::write(&f, &[7u8; 99]).unwrap();
        assert!(deep.as_os_str().len() > 4096);
        let b = crate::NativeBackend::default();
        let entries = list(&b, &deep, dfdu_sys::Storage::Unknown, 3);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].1.unwrap().apparent, 99);
        let root = b.stat_root(&deep).unwrap();
        assert_eq!(root.kind, Kind::Dir);
        // Removed level by level from the top would also fail on the long paths.
        drop(fd);
        let _ = std::process::Command::new("rm")
            .arg("-rf")
            .arg(tmp.path().join(format!("000-{}", "y".repeat(200))))
            .status();
    }
}
