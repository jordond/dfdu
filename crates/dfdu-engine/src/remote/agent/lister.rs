//! The agent's answers to [`ToAgent::List`](super::ToAgent::List) and
//! [`ToAgent::Mounts`](super::ToAgent::Mounts): each read on a thread of its own with a
//! deadline, off the control loop, so a hung mount delays only its own answer. A read that
//! does not return is left behind and answered with why; a path that cannot be listed, and a
//! thread that cannot be started, are answered with why too.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use super::super::wire::listing::{
    MAX_ENTRIES, MAX_FSTYPE, MAX_MOUNTS, MAX_NAME, MAX_PATH, clip_error,
};
use super::super::wire::{FromAgent, HostMount, ListEntry};
use super::Out;

/// How long a directory listing gets.
pub const LIST_TIMEOUT: Duration = Duration::from_secs(5);

/// What an ask says when its thread could not be started.
const NO_THREAD: &str = "cannot start a thread";

/// Lists `path` on a thread of its own and sends the answer to ask `id`. A path that is
/// empty, too long or not absolute is answered at once.
pub(super) fn list<W: Write + Send + 'static>(out: &Arc<Out<W>>, id: u64, path: Vec<u8>) {
    let listed = move |result: Result<Vec<ListEntry>, String>| FromAgent::Listed {
        id,
        result: result.map_err(clip_error),
    };
    if let Err(why) = check_path(&path) {
        out.send(&listed(Err(why.into())));
        return;
    }
    let path = PathBuf::from(dfdu_tree::name_os(&path).into_owned());
    answer(
        out,
        "dfdu-agent-list",
        listed(Err(NO_THREAD.into())),
        move || {
            let deadline = Instant::now() + LIST_TIMEOUT;
            let result = within(LIST_TIMEOUT, move || {
                read_dir(&path, MAX_ENTRIES, MAX_NAME, deadline)
            })
            .unwrap_or_else(|late| {
                Err(late.text(&format!("no answer in {} s", LIST_TIMEOUT.as_secs())))
            });
            listed(result)
        },
    );
}

/// Why `path` cannot be listed: empty, longer than [`MAX_PATH`], or not absolute on this
/// host.
fn check_path(path: &[u8]) -> Result<(), &'static str> {
    if path.is_empty() {
        return Err("no folder to list");
    }
    if path.len() > MAX_PATH {
        return Err("the path is too long");
    }
    if !dfdu_tree::HostPath::new(dfdu_tree::Host::LOCAL, path.to_vec()).is_absolute() {
        return Err("the path is not absolute");
    }
    Ok(())
}

/// Lists the mounted filesystems on a thread of its own and sends the answer to ask `id`.
/// Each filesystem gets [`dfdu_sys::DISK_TIMEOUT`] to report its size.
pub(super) fn mounts<W: Write + Send + 'static>(out: &Arc<Out<W>>, id: u64) {
    let failed = FromAgent::Mounts {
        id,
        result: Err(NO_THREAD.into()),
    };
    answer(out, "dfdu-agent-mounts", failed, move || {
        let wait = dfdu_sys::DISK_TIMEOUT + Duration::from_secs(1);
        let result = match within(wait, || dfdu_sys::disks(dfdu_sys::DISK_TIMEOUT)) {
            Ok(Ok(disks)) => Ok(host_mounts(&disks)),
            Ok(Err(e)) => Err(clip_error(e.to_string())),
            Err(late) => Err(late.text("the mount table did not answer")),
        };
        FromAgent::Mounts { id, result }
    });
}

/// Sends what `f` answers, from a thread named `name`; `failed` when the thread cannot be
/// started.
fn answer<W: Write + Send + 'static>(
    out: &Arc<Out<W>>,
    name: &str,
    failed: FromAgent,
    f: impl FnOnce() -> FromAgent + Send + 'static,
) {
    let o = out.clone();
    if thread::Builder::new()
        .name(name.into())
        .spawn(move || o.send(&f()))
        .is_err()
    {
        out.send(&failed);
    }
}

/// Why [`within`] has no value.
#[derive(Debug, PartialEq, Eq)]
enum Late {
    /// It did not return in time; it is left running.
    Timeout,
    /// Its thread could not be started.
    NoThread,
}

impl Late {
    /// What the answer says: `timeout` when it did not return in time.
    fn text(&self, timeout: &str) -> String {
        match self {
            Late::Timeout => timeout.into(),
            Late::NoThread => NO_THREAD.into(),
        }
    }
}

/// What `f` returns on a thread of its own within `timeout`.
fn within<T: Send + 'static>(
    timeout: Duration,
    f: impl FnOnce() -> T + Send + 'static,
) -> Result<T, Late> {
    let (tx, rx) = mpsc::channel();
    thread::Builder::new()
        .name("dfdu-agent-read".into())
        .spawn(move || {
            let _ = tx.send(f());
        })
        .map_err(|_| Late::NoThread)?;
    rx.recv_timeout(timeout).map_err(|_| Late::Timeout)
}

/// The entries of `dir`: directories first, at most `cap` in all; names longer than
/// `max_name` bytes are left out. Links to directories count as directories. Reading stops
/// once `cap` directories are found, or at `deadline`.
///
/// # Errors
///
/// Why the directory could not be read.
fn read_dir(
    dir: &Path,
    cap: usize,
    max_name: usize,
    deadline: Instant,
) -> Result<Vec<ListEntry>, String> {
    let mut dirs = Vec::new();
    let mut files = Vec::new();
    for e in std::fs::read_dir(dir).map_err(|e| format!("{}: {e}", dir.display()))? {
        if dirs.len() >= cap || Instant::now() >= deadline {
            break;
        }
        let Ok(e) = e else { continue };
        let name = dfdu_tree::name_bytes(&e.file_name()).into_owned();
        if name.len() > max_name {
            continue;
        }
        let is_dir = match e.file_type() {
            Ok(t) if t.is_symlink() => std::fs::metadata(e.path()).is_ok_and(|m| m.is_dir()),
            Ok(t) => t.is_dir(),
            Err(_) => false,
        };
        if is_dir {
            dirs.push(ListEntry { name, dir: true });
        } else if files.len() < cap {
            files.push(ListEntry { name, dir: false });
        }
    }
    files.truncate(cap - dirs.len());
    dirs.append(&mut files);
    Ok(dirs)
}

/// The disks of the disk list as the wire carries them, at most [`MAX_MOUNTS`]; those whose
/// mount point is empty or too long, or whose type is too long, are left out.
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
        .take(MAX_MOUNTS)
        .collect()
}

#[cfg(test)]
mod tests;
