//! What changed below a directory since an `FSEvents` event id: the history the `FSEvents`
//! daemon keeps per volume, replayed through a stream with `sinceWhen` until it reports
//! `HistoryDone`. Used to rescan only the directories that changed since a snapshot.
//!
//! Events name directories (the stream is made without file events): a file created, removed,
//! renamed or written in a directory gives an event for the directory. When the daemon dropped
//! events or coalesced them (`MustScanSubDirs`, `UserDropped`, `KernelDropped`), when ids
//! wrapped, or when the watched directory itself was moved or deleted (`RootChanged`), the
//! subtree must be scanned again in full; [`Changes::full`] says so. Event ids belong to one
//! volume: [`volume_uuid`] tells whether the volume is still the one the id came from (it is
//! not after the volume was erased or its `FSEvents` store reset).
//!
//! The daemon holds the newest events for up to a second or so before they reach the history
//! or a stream, so a replay alone can miss what changed just before it. After `HistoryDone`,
//! [`changes_since`] therefore creates a file in a directory of its own on the same volume and
//! waits for that change to arrive live: events come in the order they happened, so every
//! change before it has arrived too.

#![allow(unsafe_code)]

use std::ffi::{CStr, CString, c_char, c_void};
use std::io;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::mpsc;
use std::time::{Duration, Instant};

type CFAllocatorRef = *const c_void;
type CFArrayRef = *const c_void;
type CFStringRef = *const c_void;
type CFUUIDRef = *const c_void;
type FSEventStreamRef = *mut c_void;
type DispatchQueue = *mut c_void;

#[repr(C)]
struct FSEventStreamContext {
    version: isize,
    info: *mut c_void,
    retain: *const c_void,
    release: *const c_void,
    copy_description: *const c_void,
}

#[repr(C)]
struct CFUUIDBytes([u8; 16]);

type Callback = extern "C" fn(
    stream: FSEventStreamRef,
    info: *mut c_void,
    num_events: usize,
    paths: *mut c_void,
    flags: *const u32,
    ids: *const u64,
);

#[link(name = "CoreServices", kind = "framework")]
unsafe extern "C" {
    static kCFTypeArrayCallBacks: c_void;
    fn CFStringCreateWithFileSystemRepresentation(
        alloc: CFAllocatorRef,
        buffer: *const c_char,
    ) -> CFStringRef;
    fn CFArrayCreate(
        alloc: CFAllocatorRef,
        values: *const *const c_void,
        num: isize,
        callbacks: *const c_void,
    ) -> CFArrayRef;
    fn CFRelease(cf: *const c_void);
    fn CFUUIDGetUUIDBytes(uuid: CFUUIDRef) -> CFUUIDBytes;
    fn FSEventsGetCurrentEventId() -> u64;
    fn FSEventsCopyUUIDForDevice(dev: i32) -> CFUUIDRef;
    fn FSEventStreamCreate(
        alloc: CFAllocatorRef,
        callback: Callback,
        context: *const FSEventStreamContext,
        paths: CFArrayRef,
        since: u64,
        latency: f64,
        flags: u32,
    ) -> FSEventStreamRef;
    fn FSEventStreamSetDispatchQueue(stream: FSEventStreamRef, queue: DispatchQueue);
    fn FSEventStreamStart(stream: FSEventStreamRef) -> u8;
    fn FSEventStreamStop(stream: FSEventStreamRef);
    fn FSEventStreamInvalidate(stream: FSEventStreamRef);
    fn FSEventStreamRelease(stream: FSEventStreamRef);
    fn dispatch_queue_create(label: *const c_char, attr: *const c_void) -> DispatchQueue;
    fn dispatch_release(object: *mut c_void);
    fn dispatch_sync_f(
        queue: DispatchQueue,
        context: *mut c_void,
        work: extern "C" fn(*mut c_void),
    );
}

extern "C" fn noop(_: *mut c_void) {}

const MUST_SCAN_SUB_DIRS: u32 = 0x01;
const USER_DROPPED: u32 = 0x02;
const KERNEL_DROPPED: u32 = 0x04;
const IDS_WRAPPED: u32 = 0x08;
const HISTORY_DONE: u32 = 0x10;
const ROOT_CHANGED: u32 = 0x20;
/// `kFSEventStreamCreateFlagWatchRoot`: report the watched path itself being moved.
const CREATE_WATCH_ROOT: u32 = 0x04;
/// `kFSEventStreamEventIdSinceNow`: not a real id.
const SINCE_NOW: u64 = u64::MAX;

/// The id of the newest event; a later [`changes_since`] with it reports what changes after.
#[must_use]
pub fn current_event_id() -> u64 {
    // SAFETY: no arguments; returns a plain integer.
    unsafe { FSEventsGetCurrentEventId() }
}

/// The identity of the `FSEvents` store of device `dev` (`st_dev`); `None` when the volume
/// keeps no history (read-only, or `FSEvents` disabled on it).
#[must_use]
pub fn volume_uuid(dev: u64) -> Option<[u8; 16]> {
    #[expect(
        clippy::cast_possible_truncation,
        reason = "macOS `st_dev` is an i32 that `MetadataExt::dev` widened to u64; this undoes it"
    )]
    let dev = dev as i32;
    // SAFETY: `dev` is passed by value; the returned reference is owned by us (Copy rule) and
    // released below after its bytes are copied.
    unsafe {
        let u = FSEventsCopyUUIDForDevice(dev);
        if u.is_null() {
            return None;
        }
        let b = CFUUIDGetUUIDBytes(u).0;
        CFRelease(u);
        Some(b)
    }
}

/// What [`changes_since`] found.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Changes {
    /// Directories with changes, as the events name them (real paths, without symlinks),
    /// each once, in no particular order.
    pub dirs: Vec<PathBuf>,
    /// The history is incomplete or does not apply: scan everything again.
    pub full: bool,
    /// The newest event id seen, or the id `changes_since` was given when there was none.
    pub last_id: u64,
}

struct Collect {
    dirs: Vec<PathBuf>,
    full: bool,
    last_id: u64,
    done: Option<mpsc::Sender<()>>,
    /// Signalled when the sync file's event arrives.
    synced: Option<mpsc::Sender<()>>,
    /// The sync directory (see the module documentation); its events are not changes.
    sync: Option<Vec<u8>>,
}

extern "C" fn on_events(
    _stream: FSEventStreamRef,
    info: *mut c_void,
    n: usize,
    paths: *mut c_void,
    flags: *const u32,
    ids: *const u64,
) {
    // Nothing to read; `from_raw_parts` also needs non-null pointers even for no elements.
    if n == 0 || info.is_null() || paths.is_null() || flags.is_null() || ids.is_null() {
        return;
    }
    // SAFETY: `info` is the `Mutex<Collect>` that `changes_since` keeps alive until the stream
    // is invalidated; it is only ever borrowed shared, and the mutex serializes the writes.
    let c = unsafe { &*(info as *const Mutex<Collect>) };
    // SAFETY: without `kFSEventStreamCreateFlagUseCFTypes`, `paths` points at an array of `n`
    // C string pointers (non-null, checked above, and aligned), allocated by the daemon, so
    // its size fits in `isize`. The daemon neither frees nor writes it during the call, and
    // nothing here writes through it, so the shared slice aliases no mutable access.
    let paths = unsafe { std::slice::from_raw_parts(paths as *const *const c_char, n) };
    // SAFETY: as for `paths`: `flags` is a non-null, aligned array of `n` initialized `u32`s
    // that the daemon owns and leaves unchanged for the call; it is only read here.
    let flags = unsafe { std::slice::from_raw_parts(flags, n) };
    // SAFETY: as for `flags`, with `n` initialized `u64` event ids.
    let ids = unsafe { std::slice::from_raw_parts(ids, n) };
    let mut c = c.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    let (mut done, mut synced) = (None, None);
    for i in 0..n {
        let f = flags[i];
        if f & HISTORY_DONE != 0 {
            done = c.done.take();
            continue;
        }
        if f & (MUST_SCAN_SUB_DIRS | USER_DROPPED | KERNEL_DROPPED | IDS_WRAPPED | ROOT_CHANGED)
            != 0
        {
            c.full = true;
        }
        c.last_id = c.last_id.max(ids[i]);
        // SAFETY: see above; each entry is a valid C string for the call.
        let p = unsafe { CStr::from_ptr(paths[i]) };
        let mut p = p.to_bytes();
        while p.len() > 1 && p.ends_with(b"/") {
            p = &p[..p.len() - 1];
        }
        if c.sync.as_deref() == Some(p) {
            // Only once the history is done: the sync file is written after it.
            if c.done.is_none() && done.is_none() {
                synced = c.synced.take();
            }
            continue;
        }
        c.dirs.push(PathBuf::from(std::ffi::OsStr::from_bytes(p)));
    }
    // Only after the lock is released: the waiting thread may then free the state.
    drop(c);
    for tx in [done, synced].into_iter().flatten() {
        let _ = tx.send(());
    }
}

/// How long live events are collected after `HistoryDone` when no sync directory can be made
/// on the volume: about how long the daemon holds events.
const SETTLE: Duration = Duration::from_secs(2);

/// A directory of its own on the volume of device `dev` for the sync file, as a real path:
/// below the temporary directory when that is on the volume.
fn sync_dir(dev: u64) -> Option<PathBuf> {
    let tmp = std::fs::canonicalize(std::env::temp_dir()).ok()?;
    if std::fs::metadata(&tmp).ok()?.dev() as u64 != dev {
        return None;
    }
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.subsec_nanos());
    let d = tmp.join(format!("dfdu-fsevents-{}-{nanos}", std::process::id()));
    std::fs::create_dir(&d).ok()?;
    Some(d)
}

/// The directories at or below `root` (a real path: `FSEvents` reports paths without symlinks)
/// that changed after event `since`, up to the moment of the call. Waits at most `timeout`
/// for the daemon to replay its history and deliver the newest events; on timeout the result
/// is marked [`Changes::full`].
///
/// # Errors
///
/// `InvalidInput` when `since` is not a real event id or a path holds a NUL byte, the error of
/// reading `root`'s metadata, and an error when the stream cannot be created or started.
pub fn changes_since(root: &Path, since: u64, timeout: Duration) -> io::Result<Changes> {
    if since == SINCE_NOW {
        return Err(io::ErrorKind::InvalidInput.into());
    }
    let deadline = Instant::now() + timeout;
    let dev = std::fs::metadata(root)?.dev() as u64;
    let sync = sync_dir(dev);
    let result = replay(root, since, deadline, sync.as_deref());
    if let Some(d) = &sync {
        let _ = std::fs::remove_dir_all(d);
    }
    result
}

fn replay(root: &Path, since: u64, deadline: Instant, sync: Option<&Path>) -> io::Result<Changes> {
    let mut paths = vec![CString::new(root.as_os_str().as_bytes())?];
    if let Some(d) = sync {
        paths.push(CString::new(d.as_os_str().as_bytes())?);
    }
    let (tx, rx) = mpsc::channel();
    let (synced_tx, synced_rx) = mpsc::channel();
    let collect = Box::new(Mutex::new(Collect {
        dirs: Vec::new(),
        full: false,
        last_id: since,
        done: Some(tx),
        synced: Some(synced_tx),
        sync: sync.map(|d| d.as_os_str().as_bytes().to_vec()),
    }));
    let ctx = FSEventStreamContext {
        version: 0,
        info: (&raw const *collect) as *mut c_void,
        retain: std::ptr::null(),
        release: std::ptr::null(),
        copy_description: std::ptr::null(),
    };
    let left = || deadline.saturating_duration_since(Instant::now());
    // SAFETY: every object created here is released before returning, the stream after it is
    // stopped and invalidated and its queue drained, so no callback runs any more and
    // `collect` (which the callback borrows through `ctx.info`) outlives every use. The array
    // holds the CFStrings and retains them (`kCFTypeArrayCallBacks`); the strings are
    // NUL-terminated.
    unsafe {
        let mut values = Vec::with_capacity(paths.len());
        for p in &paths {
            let s = CFStringCreateWithFileSystemRepresentation(std::ptr::null(), p.as_ptr());
            if s.is_null() {
                for v in values {
                    CFRelease(v);
                }
                return Err(io::ErrorKind::InvalidInput.into());
            }
            values.push(s);
        }
        let array = CFArrayCreate(
            std::ptr::null(),
            values.as_ptr(),
            // At most two values.
            values.len().cast_signed(),
            &raw const kCFTypeArrayCallBacks,
        );
        for v in values {
            CFRelease(v);
        }
        if array.is_null() {
            return Err(io::ErrorKind::OutOfMemory.into());
        }
        let stream = FSEventStreamCreate(
            std::ptr::null(),
            on_events,
            &raw const ctx,
            array,
            since,
            0.0,
            CREATE_WATCH_ROOT,
        );
        CFRelease(array);
        if stream.is_null() {
            return Err(io::Error::other("FSEventStreamCreate failed"));
        }
        let queue = dispatch_queue_create(c"dfdu.fsevents".as_ptr(), std::ptr::null());
        FSEventStreamSetDispatchQueue(stream, queue);
        let started = FSEventStreamStart(stream) != 0;
        let mut finished = started && rx.recv_timeout(left()).is_ok();
        if finished {
            match sync {
                Some(d) => {
                    finished = std::fs::write(d.join("sync"), b"").is_ok()
                        && synced_rx.recv_timeout(left()).is_ok();
                }
                None => std::thread::sleep(SETTLE.min(left())),
            }
        }
        if started {
            FSEventStreamStop(stream);
        }
        FSEventStreamInvalidate(stream);
        FSEventStreamRelease(stream);
        // The callbacks run on this serial queue: once an empty item has run on it, none is
        // running or waiting.
        dispatch_sync_f(queue, std::ptr::null_mut(), noop);
        dispatch_release(queue);
        if !started {
            return Err(io::Error::other("FSEventStreamStart failed"));
        }
        let mut c = (*collect)
            .into_inner()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        c.full |= !finished;
        c.dirs.sort();
        c.dirs.dedup();
        Ok(Changes {
            dirs: c.dirs,
            full: c.full,
            last_id: c.last_id,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    /// Held by each test: the last one checks that no sync directory of this process is left,
    /// which another test's call in flight would break.
    static ONE_AT_A_TIME: Mutex<()> = Mutex::new(());

    /// Writes below a directory after an event id show up as changes of the directories they
    /// were made in; nothing before the id does.
    #[test]
    fn changes_after_an_id_are_reported() {
        let _one = ONE_AT_A_TIME
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        std::fs::create_dir_all(root.join("a/b")).unwrap();
        std::fs::create_dir_all(root.join("quiet")).unwrap();
        std::fs::write(root.join("quiet/x"), b"x").unwrap();
        // Let the writes above get ids before the one taken now.
        std::thread::sleep(Duration::from_millis(1500));
        let since = current_event_id();
        std::fs::write(root.join("a/b/new"), b"hello").unwrap();
        std::fs::write(root.join("a/top"), b"hi").unwrap();
        let mut got = Changes::default();
        // The daemon flushes events with some delay.
        for _ in 0..40 {
            got = changes_since(&root, since, Duration::from_secs(5)).unwrap();
            if got.dirs.contains(&root.join("a/b")) && got.dirs.contains(&root.join("a")) {
                break;
            }
            std::thread::sleep(Duration::from_millis(250));
        }
        assert!(got.dirs.contains(&root.join("a/b")), "{got:?}");
        assert!(got.dirs.contains(&root.join("a")), "{got:?}");
        assert!(!got.dirs.contains(&root.join("quiet")), "{got:?}");
        assert!(got.last_id > since);
        let dev = {
            use std::os::unix::fs::MetadataExt;
            std::fs::metadata(&root).unwrap().dev()
        };
        assert!(volume_uuid(dev).is_some());
    }

    /// A change made just before the call is reported, without waiting or asking again.
    #[test]
    fn a_change_just_before_the_call_is_reported() {
        let _one = ONE_AT_A_TIME
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        for i in 0..5 {
            let d = root.join(format!("d{i}"));
            std::fs::create_dir(&d).unwrap();
            let since = current_event_id();
            std::fs::write(d.join("f"), b"x").unwrap();
            let got = changes_since(&root, since, Duration::from_secs(10)).unwrap();
            assert!(!got.full);
            assert!(got.dirs.contains(&d), "{i}: {got:?}");
        }
        let left: Vec<_> = std::fs::read_dir(std::env::temp_dir().canonicalize().unwrap())
            .unwrap()
            .flatten()
            .filter(|e| {
                let n = e.file_name();
                let n = n.to_string_lossy();
                n.starts_with(&format!("dfdu-fsevents-{}-", std::process::id()))
            })
            .collect();
        assert!(left.is_empty(), "sync directories are removed");
    }
}
