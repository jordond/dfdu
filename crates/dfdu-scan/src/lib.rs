//! The scanner. Workers read directories through a [`Backend`], append everything they produce
//! to their own [`Outbox`], and make new tasks stealable only when the outbox is flushed, so
//! the applier always receives the message naming a directory before any message about it.
//!
//! The worker and watchdog logic are step functions with no threads or sleeping inside them.
//! [`runtime`] drives them with real threads; the simulation harness drives the same code in a
//! seeded single-threaded schedule.

mod backend;
mod budget;
pub mod classify;
mod gate;
pub mod late;
#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "macos")]
mod macos;
mod mounts;
pub mod ntfs;
mod proto;
pub mod runtime;
mod shared;
pub mod sync;
#[cfg(windows)]
mod windows;

pub use backend::{Backend, Block, BlockEntry, DirStream, PortableBackend, Raw, RawBlock};
pub use budget::Budget;
pub use dfdu_sys::{FsClass, Storage};
pub use gate::GateStats;
#[cfg(target_os = "linux")]
pub use linux::{LinuxBackend, LinuxStream};
#[cfg(target_os = "macos")]
pub use macos::{MacBackend, MacStream};
pub use mounts::duplicate_mounts;
#[cfg(windows)]
pub use windows::{WinLoc, WindowsBackend, WindowsStream, check_mft};

/// The fastest backend for this platform; the portable one where there is no native one.
#[cfg(target_os = "linux")]
pub type NativeBackend = LinuxBackend;
#[cfg(target_os = "macos")]
pub type NativeBackend = MacBackend;
#[cfg(windows)]
pub type NativeBackend = WindowsBackend;
#[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
pub type NativeBackend = PortableBackend;

static MFT: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Makes [`native_backend`] list NTFS volumes from their master file tables
/// ([`WindowsBackend::with_mft`]); ignored on other systems. Set once at startup, from the
/// command line.
pub fn use_mft(on: bool) {
    MFT.store(on, std::sync::atomic::Ordering::Relaxed);
}

/// The native backend set up for data on `storage` ([`dfdu_sys::storage`]).
pub fn native_backend(storage: dfdu_sys::Storage) -> NativeBackend {
    #[cfg(target_os = "linux")]
    return LinuxBackend::for_storage(storage);
    #[cfg(windows)]
    if MFT.load(std::sync::atomic::Ordering::Relaxed) {
        let _ = storage;
        return WindowsBackend::with_mft();
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = storage;
        NativeBackend::default()
    }
}
pub use proto::{
    Batch, Chunk, ChunkStatus, DirId, Entry, EntryError, FileRec, JobId, ListingStatus, Mark,
    NameSpan, Probe, ScanMsg, SkipReason, SubdirRec,
};
pub use shared::{
    Clock, JobOptions, Limits, MAX_MOUNT_WORKERS, Outbox, Shared, Sink, StepResult, Task, TaskGate,
    WatchdogReport, Worker, WorkerSlot, panic_message,
};
