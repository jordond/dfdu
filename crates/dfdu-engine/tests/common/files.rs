//! Files and directories written for a test, and their mtimes set.

use std::fs;
use std::path::Path;
#[cfg(unix)]
use std::time::Duration;

/// Writes a file of `bytes` bytes at `p`, and the directories above it.
pub fn write(p: &Path, bytes: usize) {
    fs::create_dir_all(p.parent().unwrap()).unwrap();
    fs::write(p, vec![7u8; bytes]).unwrap();
}

/// Sets the mtime of `dir` and of every directory below it to a time long past, so that any
/// change in one moves it.
#[cfg(unix)]
pub fn age(dir: &Path) {
    for e in fs::read_dir(dir).unwrap() {
        let e = e.unwrap();
        if e.file_type().unwrap().is_dir() {
            age(&e.path());
        }
    }
    set_mtime(dir, OLD);
}

/// Seconds since the epoch that [`age`] sets.
#[cfg(unix)]
pub const OLD: u64 = 1_000_000_000;

/// Sets the mtime of directory `dir`.
#[cfg(unix)]
pub fn set_mtime(dir: &Path, secs: u64) {
    let t = std::time::UNIX_EPOCH + Duration::from_secs(secs);
    fs::File::open(dir).unwrap().set_modified(t).unwrap();
}
