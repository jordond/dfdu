//! An applier fed by hand, for the unit tests of its submodules: job 1 scans `/r`, and each
//! test writes out the scanner's messages for it. Nothing runs on the filesystem.

use std::sync::Arc;

use dfdu_scan::{
    Chunk, ChunkStatus, DirId, FileRec, JobId, JobOptions, Limits, ListingStatus, NameSpan,
    PortableBackend, ScanMsg, Shared, SkipReason, SubdirRec,
};
use dfdu_tree::{DirState, Ix, Kind, Meta};

use super::{Applier, Command, JobPhase, Model};
use crate::test_util::meta;

/// The `submitted_seq` the scanner gave job 1.
pub(super) const SEQ: u64 = 1;

pub(super) struct Rig {
    pub a: Applier<PortableBackend>,
    pub m: Model,
}

impl Rig {
    /// Job 1 started on `/r`, nothing applied yet.
    pub fn scan() -> Rig {
        let shared = Arc::new(Shared::new(PortableBackend::default(), Limits::default()));
        let mut a = Applier::new(shared);
        let mut m = Model::default();
        let scan = Command::Scan {
            path: "/r".into(),
            opts: JobOptions::default(),
        };
        a.apply_command(&mut m, scan);
        Rig { a, m }
    }

    /// Job 1 with the root listed and closed, and its subdirectories `a` and `b` pending.
    pub fn two_pending() -> Rig {
        let mut r = Rig::scan();
        let l = Listing {
            subdirs: &["a", "b"],
            ..Listing::default()
        };
        r.apply(chunk(0, 0, 1, &l));
        r.apply(close(0, 1));
        r
    }

    pub fn apply(&mut self, msg: ScanMsg) {
        self.a.apply(&mut self.m, msg, 0);
    }

    pub fn command(&mut self, cmd: Command) {
        self.a.apply_command(&mut self.m, cmd);
    }

    /// Runs the queued work to the end.
    pub fn settle(&mut self) {
        while self.a.has_work() {
            self.a.work_step(&mut self.m);
        }
    }

    /// The node at `path` below the root (`""` for the root).
    ///
    /// # Panics
    ///
    /// If there is none.
    pub fn at(&self, path: &str) -> Ix {
        let t = &self.m.tree;
        let mut ix = t.roots().next().unwrap_or_else(|| panic!("no root"));
        for name in path.split('/').filter(|n| !n.is_empty()) {
            ix = t
                .children(ix)
                .find(|&c| t.name(c) == name.as_bytes())
                .unwrap_or_else(|| panic!("no {name} in {path}"));
        }
        ix
    }

    /// The state of the directory at `path`.
    ///
    /// # Panics
    ///
    /// If there is no directory there.
    pub fn state(&self, path: &str) -> DirState {
        let d = self.m.tree.dir(self.at(path));
        d.map_or_else(|| panic!("{path} is not a directory"), |d| d.state)
    }

    /// The phase of job 1.
    pub fn phase(&self) -> Option<JobPhase> {
        self.a.jobs.get(&1).map(|j| j.phase)
    }
}

/// What a chunk lists: files by name and size, then subdirectories.
#[derive(Default)]
pub(super) struct Listing<'a> {
    pub files: &'a [(&'a str, u64)],
    pub subdirs: &'a [&'a str],
    /// The subdirectories are not read, for this reason.
    pub skip: Option<SkipReason>,
}

/// Chunk `seq` of directory `dir` of job 1, its subdirectories numbered from `first`. Each
/// file has an inode of its own, on device 1.
pub(super) fn chunk(dir: u32, seq: u32, first: u32, l: &Listing) -> ScanMsg {
    let base = 1000 * u64::from(dir + 1) + 100 * u64::from(seq);
    chunk_of(dir, seq, first, l, |i, f| meta(Kind::File, f.1, base + i))
}

/// [`chunk`] with the metadata of file `i` from `file_meta`.
pub(super) fn chunk_of(
    dir: u32,
    seq: u32,
    first: u32,
    l: &Listing,
    file_meta: impl Fn(u64, &(&str, u64)) -> Meta,
) -> ScanMsg {
    let mut names = Vec::new();
    let mut span = |name: &str| {
        let s = NameSpan {
            off: u32::try_from(names.len()).unwrap_or(u32::MAX),
            len: u16::try_from(name.len()).unwrap_or(u16::MAX),
        };
        names.extend_from_slice(name.as_bytes());
        s
    };
    let files = l
        .files
        .iter()
        .zip(0..)
        .map(|(f, i)| FileRec {
            name: span(f.0),
            meta: file_meta(i, f),
        })
        .collect();
    let subdirs = l
        .subdirs
        .iter()
        .zip(u64::from(first)..)
        .map(|(d, ino)| SubdirRec {
            name: span(d),
            meta: meta(Kind::Dir, 0, ino),
            mount_root: false,
            skip: l.skip,
        })
        .collect();
    ScanMsg::Chunk(Chunk {
        job: JobId(1),
        dir: DirId(dir),
        seq,
        root_meta: None,
        probe: None,
        names,
        files,
        first_subdir: DirId(first),
        subdirs,
        errors: Vec::new(),
        marks: Vec::new(),
        status: ChunkStatus::Ok,
    })
}

/// The `Close` of directory `dir` of job 1 after `chunks` chunks, with `status`.
pub(super) fn close_with(dir: u32, chunks: u32, status: ListingStatus) -> ScanMsg {
    ScanMsg::Close {
        job: JobId(1),
        dir: DirId(dir),
        chunks,
        status,
        classes: Vec::new(),
    }
}

/// [`close_with`] a complete listing.
pub(super) fn close(dir: u32, chunks: u32) -> ScanMsg {
    close_with(dir, chunks, ListingStatus::Complete)
}
