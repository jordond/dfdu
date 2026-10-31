//! A FUSE filesystem for the demo recordings: it serves a made-up home directory described by
//! a fixture file (`demo/fixtures/*.toml`), so the recordings show the same files, sizes and
//! ages on every run and nothing of the machine they are made on.
//!
//! Sizes cost nothing: a file's length and allocated blocks are whatever the fixture says, and
//! its bytes are generated from a seed when read, so files with the same `content` read the
//! same and are found as duplicates. The tree lives in memory and can be changed (delete,
//! rename, the Trash, staging, small writes), so deleting in a recording works; remounting
//! starts again from the fixture. Selected directories answer their listing late (`delay`),
//! to make a scan visible, or not at all (`error`).
//!
//! The fixture and the tree are plain data and build on every platform; mounting is Linux only
//! (`mount`).

mod error;
mod fill;
mod fixture;
#[cfg(target_os = "linux")]
mod fs;
mod tree;

pub use error::Error;
pub use fill::{Fill, Style};
pub use fixture::{Amount, Disk, Entry, Fixture, format_time, parse_age};
#[cfg(target_os = "linux")]
pub use fs::{mount, mount_tree};
pub use tree::{Data, DirEntry, Fault, FsError, Inode, Kind, Tree};

#[cfg(test)]
mod test_util;
