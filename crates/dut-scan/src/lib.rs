//! The scanner. Workers read directories through a [`Backend`], append everything they produce
//! to their own [`Outbox`], and make new tasks stealable only when the outbox is flushed, so the
//! applier always receives the message naming a directory before any message about it
//! (`docs/data-pipeline.md` §4.3).
//!
//! The worker and watchdog logic are step functions with no threads or sleeping inside them.
//! [`runtime`] drives them with real threads; the simulation harness drives the same code in a
//! seeded single-threaded schedule.

mod backend;
mod budget;
#[cfg(target_os = "linux")]
mod linux;
mod mounts;
mod proto;
pub mod runtime;
mod shared;

pub use backend::{Backend, Block, BlockEntry, DirStream, PortableBackend, Raw, RawBlock};
pub use budget::Budget;
pub use dut_sys::FsClass;
#[cfg(target_os = "linux")]
pub use linux::{LinuxBackend, LinuxStream};
pub use mounts::duplicate_mounts;

/// The fastest backend for this platform: the Linux one on Linux, the portable one elsewhere
/// until their own backends exist (ROADMAP M2).
#[cfg(target_os = "linux")]
pub type NativeBackend = LinuxBackend;
#[cfg(not(target_os = "linux"))]
pub type NativeBackend = PortableBackend;

/// The native backend set up for data on `storage` ([`dut_sys::storage`]).
pub fn native_backend(storage: dut_sys::Storage) -> NativeBackend {
    #[cfg(target_os = "linux")]
    return LinuxBackend::for_storage(storage);
    #[cfg(not(target_os = "linux"))]
    {
        let _ = storage;
        PortableBackend::default()
    }
}
pub use proto::{
    Batch, Chunk, ChunkStatus, DirId, EntryError, FileRec, JobId, ListingStatus, NameSpan, Probe,
    ScanMsg, SkipReason, SubdirRec,
};
pub use shared::{
    Clock, JobOptions, Limits, Outbox, Shared, Sink, StepResult, Task, WatchdogReport, Worker,
    WorkerSlot, panic_message,
};
