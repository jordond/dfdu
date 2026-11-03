//! The agent's answers to [`ToAgent::List`](super::ToAgent::List) and
//! [`ToAgent::Mounts`](super::ToAgent::Mounts): each read on a thread of its own with a
//! deadline, off the control loop, so a hung mount delays only its own answer. A read that
//! does not return is left behind and answered with why.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use super::super::wire::listing::{MAX_ENTRIES, MAX_FSTYPE, MAX_NAME, MAX_PATH, clip_error};
use super::super::wire::{FromAgent, HostMount, ListEntry};
use super::Out;

/// How long a directory listing gets.
pub const LIST_TIMEOUT: Duration = Duration::from_secs(5);

/// Lists `path` on a thread of its own and sends the answer to ask `id`.
pub(super) fn list<W: Write + Send + 'static>(out: &Arc<Out<W>>, id: u64, path: Vec<u8>) {
    let path = PathBuf::from(dfdu_tree::name_os(&path).into_owned());
    answer(out, "dfdu-agent-list", move || {
        let result = within(LIST_TIMEOUT, move || read_dir(&path))
            .unwrap_or_else(|| Err(format!("no answer in {} s", LIST_TIMEOUT.as_secs())));
        FromAgent::Listed {
            id,
            result: result.map_err(clip_error),
        }
    });
}

/// Lists the mounted filesystems on a thread of its own and sends the answer to ask `id`.
/// Each filesystem gets [`dfdu_sys::DISK_TIMEOUT`] to report its size.
pub(super) fn mounts<W: Write + Send + 'static>(out: &Arc<Out<W>>, id: u64) {
    answer(out, "dfdu-agent-mounts", move || {
        let wait = dfdu_sys::DISK_TIMEOUT + Duration::from_secs(1);
        let result = within(wait, || dfdu_sys::disks(dfdu_sys::DISK_TIMEOUT))
            .unwrap_or_else(|| Err(std::io::Error::other("the mount table did not answer")))
            .map(|disks| host_mounts(&disks))
            .map_err(|e| clip_error(e.to_string()));
        FromAgent::Mounts { id, result }
    });
}

/// Sends what `f` answers, from a thread named `name`.
fn answer<W: Write + Send + 'static>(
    out: &Arc<Out<W>>,
    name: &str,
    f: impl FnOnce() -> FromAgent + Send + 'static,
) {
    let o = out.clone();
    // A thread that cannot be started leaves the ask unanswered until the link ends; the
    // local side's prompt shows nothing for it.
    let _ = thread::Builder::new()
        .name(name.into())
        .spawn(move || o.send(&f()));
}

/// What `f` returns on a thread of its own within `timeout`; `None` when it did not return in
/// time (it is left running) or the thread could not be started.
fn within<T: Send + 'static>(
    timeout: Duration,
    f: impl FnOnce() -> T + Send + 'static,
) -> Option<T> {
    let (tx, rx) = mpsc::channel();
    thread::Builder::new()
        .name("dfdu-agent-read".into())
        .spawn(move || {
            let _ = tx.send(f());
        })
        .ok()?;
    rx.recv_timeout(timeout).ok()
}

/// The entries of `dir`: directories first, at most [`MAX_ENTRIES`] in all; names longer
/// than [`MAX_NAME`] are left out. Links to directories count as directories.
///
/// # Errors
///
/// Why the directory could not be read.
pub fn read_dir(dir: &Path) -> Result<Vec<ListEntry>, String> {
    let mut dirs = Vec::new();
    let mut files = Vec::new();
    for e in std::fs::read_dir(dir).map_err(|e| format!("{}: {e}", dir.display()))? {
        let Ok(e) = e else { continue };
        let name = dfdu_tree::name_bytes(&e.file_name()).into_owned();
        if name.len() > MAX_NAME {
            continue;
        }
        let is_dir = match e.file_type() {
            Ok(t) if t.is_symlink() => std::fs::metadata(e.path()).is_ok_and(|m| m.is_dir()),
            Ok(t) => t.is_dir(),
            Err(_) => false,
        };
        if is_dir {
            if dirs.len() < MAX_ENTRIES {
                dirs.push(ListEntry { name, dir: true });
            }
        } else if files.len() < MAX_ENTRIES {
            files.push(ListEntry { name, dir: false });
        }
    }
    files.truncate(MAX_ENTRIES - dirs.len());
    dirs.append(&mut files);
    Ok(dirs)
}

/// The disks of the disk list as the wire carries them, at most
/// [`MAX_MOUNTS`](super::super::wire::listing::MAX_MOUNTS); those whose mount point or type
/// is too long are left out.
fn host_mounts(disks: &[dfdu_sys::Disk]) -> Vec<HostMount> {
    (disks.iter())
        .map(|d| HostMount {
            point: dfdu_tree::name_bytes(d.mount_point.as_os_str()).into_owned(),
            fstype: d.fstype.clone(),
            used: d.space.map(|s| s.used),
            total: d.space.map(|s| s.total),
        })
        .filter(|m| {
            m.point.len() <= MAX_PATH && m.fstype.len() <= MAX_FSTYPE && !m.point.is_empty()
        })
        .take(super::super::wire::listing::MAX_MOUNTS)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_listing_puts_directories_first_and_stops_at_the_cap() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir(tmp.path().join("sub")).unwrap();
        std::fs::write(tmp.path().join("file"), b"").unwrap();
        let got = read_dir(tmp.path()).unwrap();
        assert_eq!(
            got,
            [
                ListEntry {
                    name: b"sub".to_vec(),
                    dir: true
                },
                ListEntry {
                    name: b"file".to_vec(),
                    dir: false
                },
            ]
        );
        assert!(read_dir(&tmp.path().join("gone")).is_err());
    }

    #[test]
    fn a_read_that_does_not_return_is_left_behind() {
        let got = within(Duration::from_millis(20), || {
            thread::sleep(Duration::from_secs(2));
        });
        assert!(got.is_none());
        assert_eq!(within(Duration::from_secs(5), || 7), Some(7));
    }
}
