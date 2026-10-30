//! Change journals: which directories changed since a snapshot was taken (`FSEvents` on macOS,
//! the USN journal on Windows, btrfs generations on Linux as root),
//! so a resume lists only those again (the engine's `Command::Resume`).

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;

use crate::sync::Mutex;

/// How long the `FSEvents` daemon may take to replay its history.
#[cfg(target_os = "macos")]
const REPLAY: std::time::Duration = std::time::Duration::from_secs(30);

/// Where macOS mounts the Data volume, which the firmlinks of the system volume join.
#[cfg(any(target_os = "macos", test))]
const DATA_VOLUME: &str = "/System/Volumes/Data";

/// Directories a resume keeps from the tree it loaded, by device and inode, with the mtime
/// each was loaded with ([`JobOptions::keep`](crate::JobOptions::keep)): the journal says
/// their entries did not change, so a scan that meets one with that mtime does not read it but
/// reports it skipped as [`SkipReason::Kept`](crate::SkipReason::Kept), at most once, and the
/// applier puts the loaded subtree in its place. One whose mtime changed is read: its entries
/// changed whatever the journal said (an event it lost, or another directory that reuses the
/// inode).
pub struct Keep(Mutex<HashMap<(u64, u64), i64>>);

impl Keep {
    #[must_use]
    pub fn new(dirs: impl IntoIterator<Item = ((u64, u64), i64)>) -> Keep {
        Keep(Mutex::new(dirs.into_iter().collect()))
    }

    /// Whether directory `(dev, ino)`, now with `mtime`, is kept; true for the first scan of
    /// it to ask only.
    pub fn take(&self, dev: u64, ino: u64, mtime: i64) -> bool {
        self.0.lock().remove(&(dev, ino)) == Some(mtime)
    }
}

impl std::fmt::Debug for Keep {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Keep")
    }
}

/// A position in a volume's change journal (`FSEvents` on macOS, the USN journal on Windows,
/// a btrfs subvolume's generation on Linux), and when it was read.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct JournalPosition {
    /// The journal's identity (the `FSEvents` store UUID; the USN journal id and volume serial;
    /// the btrfs subvolume uuid): positions in another are meaningless.
    pub volume: [u8; 16],
    /// The last event (on btrfs, the subvolume's generation).
    pub event_id: u64,
    /// Seconds since the Unix epoch when the position was read, 0 when not known. btrfs
    /// resumes count an inode as changed only if its ctime is not older, so atime updates (the
    /// scan's own among them) do not. A resume is sent it (the engine's `Command::Resume`
    /// `since`): a loaded directory, the root among them, whose mtime is not older is listed
    /// again whatever its stat says, since a change in that same second after the scan read it
    /// leaves the mtime as it was.
    pub time: u64,
}

/// The directories at or below `root` that changed after the journal position `at`, relative
/// to `root` (the root itself is the empty path). `None` when the journal cannot tell: `root`
/// is on another volume, the history was lost or coalesced, or the system keeps no journal;
/// everything must then be scanned again. On macOS a change made in the seconds before the call
/// on a loaded machine can be missing, `FSEvents` holding it longer than the call waits; a
/// position taken before the call ([`journal_position`]) has it.
#[must_use]
pub fn changed_since(root: &Path, at: JournalPosition) -> Option<Vec<PathBuf>> {
    changed(root, at, None)
}

/// [`changed_since`], given up with `None` as soon as `cancel` is set: the `FSEvents` replay,
/// which can take seconds, looks at it every 50 ms and removes its sync directory on the way
/// out. The USN journal and btrfs are read without looking at it.
#[must_use]
pub fn changed_since_cancellable(
    root: &Path,
    at: JournalPosition,
    cancel: &AtomicBool,
) -> Option<Vec<PathBuf>> {
    changed(root, at, Some(cancel))
}

fn changed(root: &Path, at: JournalPosition, cancel: Option<&AtomicBool>) -> Option<Vec<PathBuf>> {
    let JournalPosition {
        volume,
        event_id,
        time,
    } = at;
    #[cfg(not(target_os = "linux"))]
    let _ = time;
    #[cfg(not(target_os = "macos"))]
    let _ = cancel;
    #[cfg(target_os = "macos")]
    {
        use std::os::unix::fs::MetadataExt;
        let root = dunce::canonicalize(root).ok()?;
        let dev = std::fs::metadata(&root).ok()?.dev() as u64;
        if dfdu_sys::fsevents::volume_uuid(dev)? != volume {
            return None;
        }
        let c = match cancel {
            Some(cancel) => {
                dfdu_sys::fsevents::changes_since_cancellable(&root, event_id, REPLAY, cancel)
            }
            None => dfdu_sys::fsevents::changes_since(&root, event_id, REPLAY),
        }
        .ok()?;
        if c.full {
            return None;
        }
        // Events name what is in the Data volume by its firmlinked paths (`/Users/me`), the
        // stream watching either form: both sides are compared in the Data volume's form,
        // which is where a tree of `/` has them. A root named in that form
        // (`/System/Volumes/Data/Users/me`) can get a change made within a second or so before
        // the call only on a later call: the sync file, written through the firmlink, does not
        // vouch for a stream watching that form.
        let links = std::fs::read_to_string("/usr/share/firmlinks")
            .map(|t| firmlinks(&t))
            .unwrap_or_default();
        let real = through_firmlinks(&root, &links);
        Some(
            c.dirs
                .iter()
                .filter_map(|d| {
                    through_firmlinks(d, &links)
                        .strip_prefix(&real)
                        .ok()
                        .map(Path::to_path_buf)
                })
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

/// The firmlinks of `/usr/share/firmlinks` (macOS 10.15): each directory of the system volume
/// and the directory of the Data volume it joins.
#[cfg(any(target_os = "macos", test))]
fn firmlinks(text: &str) -> Vec<(PathBuf, PathBuf)> {
    text.lines()
        .filter_map(|l| l.split_once('\t'))
        .filter(|(on_root, _)| on_root.starts_with('/'))
        .map(|(on_root, data)| (PathBuf::from(on_root), Path::new(DATA_VOLUME).join(data)))
        .collect()
}

/// `path` with the longest firmlink it starts with replaced by the Data volume directory the
/// firmlink joins; as it is when it starts with none.
#[cfg(any(target_os = "macos", test))]
fn through_firmlinks(path: &Path, links: &[(PathBuf, PathBuf)]) -> PathBuf {
    links
        .iter()
        .filter(|(on_root, _)| path.starts_with(on_root))
        .max_by_key(|(on_root, _)| on_root.components().count())
        .and_then(|(on_root, data)| {
            let rest = path.strip_prefix(on_root).ok()?;
            Some(if rest.as_os_str().is_empty() {
                data.clone()
            } else {
                data.join(rest)
            })
        })
        .unwrap_or_else(|| path.to_path_buf())
}

/// Where the change journal of `path`'s volume stands, for [`changed_since`]; `None` where
/// there is none (on Linux: off btrfs, or without `CAP_SYS_ADMIN`). On macOS the position is
/// well back ([`dfdu_sys::fsevents::position_event_id`]), so that a change folded into an event
/// the `FSEvents` daemon still held is replayed.
#[must_use]
pub fn journal_position(path: &Path) -> Option<JournalPosition> {
    // Read before the position, so that whatever changes after it has a ctime no older.
    let time = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    let (volume, event_id) = position(path)?;
    Some(JournalPosition {
        volume,
        event_id,
        time,
    })
}

/// The journal identity and position of `path`'s volume.
fn position(path: &Path) -> Option<([u8; 16], u64)> {
    #[cfg(target_os = "macos")]
    {
        use std::os::unix::fs::MetadataExt;
        let dev = std::fs::metadata(path).ok()?.dev() as u64;
        let volume = dfdu_sys::fsevents::volume_uuid(dev)?;
        // Not the bare current id: a change made soon after it to a directory the daemon still
        // holds an event for would carry an older id, and be lost to the resume.
        Some((volume, dfdu_sys::fsevents::position_event_id()))
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

#[cfg(test)]
mod firmlink_tests {
    use super::*;

    /// Paths through a firmlink become the Data volume's, by the longest firmlink; others stay.
    #[test]
    fn firmlinked_paths_are_named_in_the_data_volume() {
        let links = firmlinks(
            "/Users\tUsers\n/System/Library/Caches\tSystem/Library/Caches\nbad line\n/private\tprivate\n",
        );
        assert_eq!(links.len(), 3);
        let data = |p: &str| through_firmlinks(Path::new(p), &links);
        assert_eq!(
            data("/Users/me/x"),
            Path::new("/System/Volumes/Data/Users/me/x")
        );
        assert_eq!(data("/Users"), Path::new("/System/Volumes/Data/Users"));
        assert_eq!(
            data("/System/Library/Caches/c"),
            Path::new("/System/Volumes/Data/System/Library/Caches/c")
        );
        assert_eq!(
            data("/System/Library/Fonts"),
            Path::new("/System/Library/Fonts")
        );
        assert_eq!(data("/Usersx"), Path::new("/Usersx"));
        assert_eq!(data("/"), Path::new("/"));
        // A root above the Data volume, at it, and below a firmlink see the same change.
        let change = data("/Users/me/x");
        assert_eq!(
            change.strip_prefix(data("/")).unwrap(),
            Path::new("System/Volumes/Data/Users/me/x")
        );
        assert_eq!(
            change
                .strip_prefix(data("/System/Volumes/Data/Users"))
                .unwrap(),
            Path::new("me/x")
        );
        assert_eq!(
            change.strip_prefix(data("/Users/me")).unwrap(),
            Path::new("x")
        );
    }

    /// A kept directory is handed out once, and only with the mtime it was loaded with.
    #[test]
    fn a_kept_directory_is_taken_once() {
        let k = Keep::new([((1, 2), 10), ((1, 3), 10), ((1, 4), 10)]);
        assert!(k.take(1, 2, 10));
        assert!(!k.take(1, 2, 10));
        assert!(!k.take(2, 2, 10));
        assert!(k.take(1, 3, 10));
        assert!(!k.take(1, 4, 11), "its entries changed");
        assert!(!k.take(1, 4, 10));
    }
}

#[cfg(all(test, target_os = "macos"))]
mod tests {
    use super::*;
    use std::os::unix::fs::MetadataExt;

    /// Directories changed after the position come back relative to the root, also those
    /// written to just before it was taken, from the first answer; a position in another
    /// volume's journal tells nothing.
    #[test]
    fn changes_come_back_relative_to_the_root() {
        let tmp = tempfile::tempdir().unwrap();
        // Not canonical on purpose (/var is a symlink): FSEvents names real paths.
        let root = tmp.path().join("r");
        std::fs::create_dir_all(root.join("a/b")).unwrap();
        std::fs::create_dir_all(root.join("quiet")).unwrap();
        let dev = std::fs::metadata(&root).unwrap().dev() as u64;
        if dfdu_sys::fsevents::volume_uuid(dev).is_none() {
            return;
        }
        let since = journal_position(&root).expect("a position");
        std::fs::write(root.join("a/b/new"), b"x").unwrap();
        // A loaded machine can keep the daemon past the replay's wait, and the journal then
        // cannot tell (a full rescan): asked again. An answer that leaves the change out is
        // not.
        let answer = |root: &Path| {
            (0..5)
                .find_map(|_| changed_since(root, since))
                .expect("the journal can tell")
        };
        // `quiet` changed just before the position, which reaches a few seconds back: it may
        // be named too.
        let got = answer(&root);
        assert!(got.contains(&PathBuf::from("a/b")), "{got:?}");
        // The same directory named in the Data volume, where events name it through the
        // `/private` firmlink. The sync file is written through the firmlink, so it does not
        // vouch for a stream watching the Data volume's form: a change made just before shows
        // up there up to a second or two late.
        let real = dunce::canonicalize(&root).unwrap();
        let data = Path::new(DATA_VOLUME).join(real.strip_prefix("/").unwrap());
        if data.exists() {
            let mut got = answer(&data);
            for _ in 0..20 {
                if got.contains(&PathBuf::from("a/b")) {
                    break;
                }
                std::thread::sleep(std::time::Duration::from_millis(250));
                got = answer(&data);
            }
            assert!(got.contains(&PathBuf::from("a/b")), "{got:?}");
        }
        let mut other = since;
        other.volume[0] ^= 0xff;
        assert_eq!(changed_since(&root, other), None);
    }
}
