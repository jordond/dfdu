//! Change journals: which directories changed since a snapshot was taken (`FSEvents` on macOS,
//! the USN journal on Windows, btrfs generations on Linux as root),
//! so a resume rescans only those ([`Command::Resume`](crate::Command::Resume)).

use std::path::{Path, PathBuf};

/// How long the `FSEvents` daemon may take to replay its history.
#[cfg(target_os = "macos")]
const REPLAY: std::time::Duration = std::time::Duration::from_secs(30);

/// The directories at or below `root` that changed after event `event_id` of the journal
/// `volume`, relative to `root` (the root itself is the empty path). `None` when the journal
/// cannot tell: `root` is on another volume, the history was lost or coalesced, or the system
/// keeps no journal; everything must then be scanned again. `time` is when the position was
/// read (seconds since the Unix epoch, 0 when not known); btrfs needs it to tell atime updates
/// from changes.
#[must_use]
pub fn changed_since(
    root: &Path,
    volume: [u8; 16],
    event_id: u64,
    time: u64,
) -> Option<Vec<PathBuf>> {
    #[cfg(not(target_os = "linux"))]
    let _ = time;
    #[cfg(target_os = "macos")]
    {
        use std::os::unix::fs::MetadataExt;
        let root = dunce::canonicalize(root).ok()?;
        let dev = std::fs::metadata(&root).ok()?.dev() as u64;
        if dfdu_sys::fsevents::volume_uuid(dev)? != volume {
            return None;
        }
        let c = dfdu_sys::fsevents::changes_since(&root, event_id, REPLAY).ok()?;
        if c.full {
            return None;
        }
        Some(
            c.dirs
                .iter()
                .filter_map(|d| d.strip_prefix(&root).ok().map(Path::to_path_buf))
                .collect(),
        )
    }
    #[cfg(windows)]
    {
        let c = dfdu_sys::usn_changes_since(root, volume, event_id).ok()?;
        (!c.full).then_some(c.dirs)
    }
    #[cfg(target_os = "linux")]
    {
        let c = dfdu_sys::btrfs_changes_since(root, volume, event_id, time).ok()?;
        (!c.full).then_some(c.dirs)
    }
    #[cfg(not(any(target_os = "macos", windows, target_os = "linux")))]
    {
        let _ = (root, volume, event_id);
        None
    }
}

/// Where the change journal of `path`'s volume stands, as `(journal identity, position)` for
/// [`changed_since`]; `None` where there is none (on Linux: off btrfs, or without
/// `CAP_SYS_ADMIN`).
#[must_use]
pub fn journal_position(path: &Path) -> Option<([u8; 16], u64)> {
    #[cfg(target_os = "macos")]
    {
        use std::os::unix::fs::MetadataExt;
        let dev = std::fs::metadata(path).ok()?.dev() as u64;
        let volume = dfdu_sys::fsevents::volume_uuid(dev)?;
        Some((volume, dfdu_sys::fsevents::current_event_id()))
    }
    #[cfg(windows)]
    {
        dfdu_sys::usn_position(path).ok()
    }
    #[cfg(target_os = "linux")]
    {
        dfdu_sys::btrfs_position(path).ok()
    }
    #[cfg(not(any(target_os = "macos", windows, target_os = "linux")))]
    {
        let _ = path;
        None
    }
}

#[cfg(all(test, target_os = "macos"))]
mod tests {
    use super::*;
    use std::os::unix::fs::MetadataExt;
    use std::time::Duration;

    /// Directories changed after the position come back relative to the root; a position in
    /// another volume's journal tells nothing.
    #[test]
    fn changes_come_back_relative_to_the_root() {
        let tmp = tempfile::tempdir().unwrap();
        // Not canonical on purpose (/var is a symlink): FSEvents names real paths.
        let root = tmp.path().join("r");
        std::fs::create_dir_all(root.join("a/b")).unwrap();
        std::fs::create_dir_all(root.join("quiet")).unwrap();
        std::thread::sleep(Duration::from_millis(1500));
        let dev = std::fs::metadata(&root).unwrap().dev() as u64;
        let Some(volume) = dfdu_sys::fsevents::volume_uuid(dev) else {
            return;
        };
        let since = dfdu_sys::fsevents::current_event_id();
        std::fs::write(root.join("a/b/new"), b"x").unwrap();
        let mut got = None;
        for _ in 0..40 {
            got = changed_since(&root, volume, since, 0);
            if got
                .as_ref()
                .is_some_and(|d| d.contains(&PathBuf::from("a/b")))
            {
                break;
            }
            std::thread::sleep(Duration::from_millis(250));
        }
        let got = got.expect("the journal can tell");
        assert!(got.contains(&PathBuf::from("a/b")), "{got:?}");
        assert!(!got.iter().any(|d| d.starts_with("quiet")), "{got:?}");
        let mut other = volume;
        other[0] ^= 0xff;
        assert_eq!(changed_since(&root, other, since, 0), None);
    }
}
