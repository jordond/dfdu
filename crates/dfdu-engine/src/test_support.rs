//! Helpers for the tests of this crate and of the crates above it (the `test-support` feature):
//! a gate that holds threads until it is opened, and a wide tree of about a million entries for
//! the measurements of saving and opening scans.

use std::path::Path;
use std::sync::{Condvar, Mutex, PoisonError};

use dfdu_tree::{DirState, Kind, Meta, NodeFlags, Tree};

/// A gate threads wait at until it is opened: a change journal or a filesystem call that
/// answers late. `const` so that a test can hold one in a static, for a plain `fn` to wait at.
pub struct Gate {
    open: Mutex<bool>,
    opened: Condvar,
}

impl Gate {
    #[must_use]
    pub const fn new() -> Gate {
        Gate {
            open: Mutex::new(false),
            opened: Condvar::new(),
        }
    }

    /// Waits until the gate is opened; at once when it is already.
    pub fn wait(&self) {
        let mut open = self.open.lock().unwrap_or_else(PoisonError::into_inner);
        while !*open {
            open = (self.opened.wait(open)).unwrap_or_else(PoisonError::into_inner);
        }
    }

    /// Opens the gate for good, letting every waiting thread through.
    pub fn open(&self) {
        *self.open.lock().unwrap_or_else(PoisonError::into_inner) = true;
        self.opened.notify_all();
    }
}

impl Default for Gate {
    fn default() -> Gate {
        Gate::new()
    }
}

/// The tree of the directory `root`, its stat `meta`: `dirs` directories `dir-00000` and on,
/// each with `files` files `file-000.dat` and on of 0, 1000, 2000... bytes, 4 KiB on disk, every
/// directory read to the end. 10 000 directories of 100 files are the million entries the
/// measurements of saving and opening a scan take.
#[must_use]
pub fn wide_tree(root: &Path, meta: &Meta, dirs: u64, files: u64) -> Tree {
    let mut t = Tree::new();
    let r = t.new_root(root.as_os_str().as_encoded_bytes(), meta, true);
    let complete = |t: &mut Tree, d| {
        if let Some(rec) = t.dir_mut(d) {
            rec.state = DirState::Complete;
        }
    };
    complete(&mut t, r);
    let mut ino = 10;
    for d in 0..dirs {
        ino += 1;
        let dm = Meta { ino, ..*meta };
        let dir = t.add_dir_flagged(
            r,
            format!("dir-{d:05}").as_bytes(),
            &dm,
            NodeFlags::default(),
        );
        complete(&mut t, dir);
        let own = t.own(dir);
        t.propagate_add(r, &own);
        for f in 0..files {
            ino += 1;
            let m = Meta {
                kind: Kind::File,
                apparent: f * 1000,
                alloc: 4096,
                ino,
                nlink: 1,
                ..*meta
            };
            let x = t.add_file(dir, format!("file-{f:03}.dat").as_bytes(), &m, true, None);
            let own = t.own(x);
            t.propagate_add(dir, &own);
        }
    }
    t
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::thread;

    use super::*;
    use crate::test_util::meta;

    #[test]
    fn a_gate_holds_its_waiters_until_opened() {
        let gate = Arc::new(Gate::new());
        let (tx, rx) = std::sync::mpsc::channel();
        let waiter = {
            let gate = Arc::clone(&gate);
            thread::spawn(move || {
                gate.wait();
                tx.send(()).unwrap();
            })
        };
        assert!(
            rx.recv_timeout(std::time::Duration::from_millis(50))
                .is_err(),
            "held while closed"
        );
        gate.open();
        rx.recv().unwrap();
        waiter.join().unwrap();
        gate.wait();
    }

    #[test]
    fn the_wide_tree_has_its_directories_and_files() {
        let t = wide_tree(Path::new("/r"), &meta(Kind::Dir, 0, 1), 3, 4);
        let r = t.roots().next().unwrap();
        let names: Vec<&[u8]> = t.children(r).map(|c| t.name(c)).collect();
        assert_eq!(names, [&b"dir-00000"[..], b"dir-00001", b"dir-00002"]);
        let first = t.children(r).next().unwrap();
        assert!(t.dir(first).is_some_and(|d| d.state == DirState::Complete));
        let sizes: Vec<u64> = t.children(first).map(|f| t.raw_size(f).0).collect();
        assert_eq!(sizes, [0, 1000, 2000, 3000]);
        assert_eq!(t.total(r).alloc, 3 * 4 * 4096);
    }
}
