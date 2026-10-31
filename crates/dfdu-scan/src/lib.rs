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
pub mod journal;
pub mod late;
mod mounts;
mod proto;
pub mod runtime;
mod shared;
pub mod sync;

pub use backend::ntfs;
pub use backend::{Backend, Block, BlockEntry, DirStream, Forward, PortableBackend, Raw, RawBlock};
#[cfg(target_os = "linux")]
pub use backend::{LinuxBackend, LinuxStream};
#[cfg(target_os = "macos")]
pub use backend::{MacBackend, MacStream};
#[cfg(windows)]
pub use backend::{WinLoc, WindowsBackend, WindowsStream, check_mft};
pub use budget::Budget;
pub use gate::GateStats;
pub use mounts::duplicate_mounts;

/// The fastest backend for this platform; the portable one where there is no native one.
#[cfg(target_os = "linux")]
pub type NativeBackend = LinuxBackend;
#[cfg(target_os = "macos")]
pub type NativeBackend = MacBackend;
#[cfg(windows)]
pub type NativeBackend = WindowsBackend;
#[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
pub type NativeBackend = PortableBackend;

/// The native backend set up for data on `storage` ([`dfdu_sys::storage`]). With `mft`, NTFS
/// volumes are listed from their master file tables (`WindowsBackend::with_mft`); `mft` is
/// ignored on other systems.
#[must_use]
pub fn native_backend(storage: dfdu_sys::Storage, mft: bool) -> NativeBackend {
    #[cfg(target_os = "linux")]
    {
        let _ = mft;
        LinuxBackend::for_storage(storage)
    }
    #[cfg(windows)]
    {
        let _ = storage;
        if mft {
            WindowsBackend::with_mft()
        } else {
            WindowsBackend::default()
        }
    }
    #[cfg(not(any(target_os = "linux", windows)))]
    {
        let _ = (storage, mft);
        NativeBackend::default()
    }
}
pub use proto::{
    Batch, Chunk, ChunkStatus, DirId, Entry, EntryError, FileRec, JobId, ListingStatus, Mark,
    NameSpan, Probe, ScanMsg, SkipReason, SubdirRec,
};
pub use shared::{
    Clock, JobOptions, Limits, MAX_MOUNT_WORKERS, Outbox, RunningPart, Shared, Sink, StepResult,
    Task, TaskGate, VerifyDir, WatchdogReport, Worker, WorkerSlot, panic_message,
};
