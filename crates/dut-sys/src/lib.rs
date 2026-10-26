//! OS calls that no safe crate provides, behind safe wrappers (`docs/btrfs.md` §3.8). Syscalls
//! with safe rustix APIs are called directly; only the btrfs ioctls need `unsafe`, each in one
//! block with its argument struct checked against the kernel's size.
//!
//! On other systems the functions report "not available" (`Ok(None)` or `Unsupported`).

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "linux")]
pub use linux::*;

#[cfg(not(target_os = "linux"))]
mod other;
#[cfg(not(target_os = "linux"))]
pub use other::*;

/// A btrfs subvolume root, as `BTRFS_IOC_GET_SUBVOL_INFO` reports it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SubvolInfo {
    pub id: u64,
    /// Id of the subvolume this one's root directory is in (5 for the top level).
    pub parent_id: u64,
    pub name: Vec<u8>,
    pub uuid: [u8; 16],
    /// Set for snapshots: the uuid of the subvolume the snapshot was taken of.
    pub parent_uuid: Option<[u8; 16]>,
    pub read_only: bool,
    /// Creation time, seconds since the Unix epoch.
    pub otime: i64,
}

/// One line of `/proc/self/mountinfo`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MountEntry {
    pub id: u64,
    pub parent: u64,
    /// Device of the filesystem (`major:minor`); the same for every mount of it.
    pub dev: (u32, u32),
    /// Directory of the filesystem shown at the mount point (`/sub` for a subvolume or bind
    /// mount of `sub`).
    pub root: std::path::PathBuf,
    pub mount_point: std::path::PathBuf,
}

/// Parses `/proc/self/mountinfo` text. Malformed lines are skipped.
pub fn parse_mountinfo(text: &[u8]) -> Vec<MountEntry> {
    text.split(|&b| b == b'\n')
        .filter_map(|line| {
            let mut f = line.split(|&b| b == b' ');
            let id = std::str::from_utf8(f.next()?).ok()?.parse().ok()?;
            let parent = std::str::from_utf8(f.next()?).ok()?.parse().ok()?;
            let (maj, min) = std::str::from_utf8(f.next()?).ok()?.split_once(':')?;
            let dev = (maj.parse().ok()?, min.parse().ok()?);
            let root = unescape(f.next()?)?;
            let mount_point = unescape(f.next()?)?;
            Some(MountEntry {
                id,
                parent,
                dev,
                root,
                mount_point,
            })
        })
        .collect()
}

/// Octal escapes (`\040` for a space) as the kernel writes them.
fn unescape(field: &[u8]) -> Option<std::path::PathBuf> {
    let mut out = Vec::with_capacity(field.len());
    let mut i = 0;
    while i < field.len() {
        if field[i] == b'\\' && i + 3 < field.len() {
            let oct = std::str::from_utf8(&field[i + 1..i + 4]).ok()?;
            out.push(u8::from_str_radix(oct, 8).ok()?);
            i += 4;
        } else {
            out.push(field[i]);
            i += 1;
        }
    }
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStringExt;
        Some(std::ffi::OsString::from_vec(out).into())
    }
    #[cfg(not(unix))]
    {
        String::from_utf8(out).ok().map(Into::into)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mountinfo_lines() {
        let text = b"50 28 0:41 / /srv/b\\040t rw,relatime shared:1 - btrfs /dev/loop0 rw\n\
                     51 50 0:41 /sub /srv/b\\040t/mnt rw - btrfs /dev/loop0 rw\nbroken\n";
        let m = parse_mountinfo(text);
        assert_eq!(m.len(), 2);
        assert_eq!(m[0].mount_point, std::path::Path::new("/srv/b t"));
        assert_eq!((m[1].id, m[1].parent, m[1].dev), (51, 50, (0, 41)));
        assert_eq!(m[1].root, std::path::Path::new("/sub"));
        assert_eq!(m[1].mount_point, std::path::Path::new("/srv/b t/mnt"));
    }
}
