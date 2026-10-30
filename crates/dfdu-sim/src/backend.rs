//! The scanner's side of the simulation: a backend, clock and sink over a [`Spec`], with the
//! faults injected where the spec places them.

use std::collections::{HashSet, VecDeque};
use std::io;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering::SeqCst};

use dfdu_engine::{MeasureMsg, MeasureReq, MeasureResult, Measurement};
use dfdu_scan::{Backend, Batch, Block, Budget, Clock, DirStream, Raw, RawBlock, ScanMsg, Sink};
use dfdu_sys::{FsClass, Storage};
use dfdu_tree::{ErrKind, Kind, Meta, Subvol};
use parking_lot::Mutex;

use crate::{Fault, ROOT, ROOT_NAME, Spec};

/// The key the scanner gives a directory's mount: its mount id, or its device without ids.
fn mount_key_of(mount: u64, dev: u64) -> (u64, u64) {
    if mount != 0 { (mount, 0) } else { (0, dev) }
}

pub(crate) fn mount_key(m: &Meta) -> (u64, u64) {
    mount_key_of(m.mount, m.dev)
}

pub struct SimClock(pub(crate) AtomicU64);

impl SimClock {
    pub fn advance(&self, ns: u64) {
        self.0.fetch_add(ns, SeqCst);
    }
}

impl Clock for SimClock {
    fn now(&self) -> u64 {
        self.0.load(SeqCst)
    }
}

pub struct SimBackend {
    pub spec: Arc<Spec>,
    pub clock: Arc<SimClock>,
    /// Directories opened with a storage class other than their mount's: the scanner
    /// hands the class of the directory's mount to `open`, whichever mount the scan began on.
    pub wrong_storage: AtomicU64,
}

pub struct SimStream {
    spec: Arc<Spec>,
    clock: Arc<SimClock>,
    node: u32,
    /// Live children when the directory was opened.
    children: Vec<u32>,
    pos: usize,
    stall_start: u64,
    stall_done: bool,
    eio_pending: bool,
}

impl DirStream for SimStream {
    type Loc = u32;

    fn next_raw(&mut self, max_entries: usize, _max_bytes: usize) -> io::Result<(Raw<u32>, bool)> {
        let n = &self.spec.nodes[self.node as usize];
        let block_idx = self.pos / max_entries;
        if self.eio_pending {
            return Err(io::Error::from_raw_os_error(5));
        }
        match n.fault {
            Fault::Stall { block, dur_ns } if block == block_idx && !self.stall_done => {
                let now = self.clock.now();
                if self.stall_start == 0 {
                    self.stall_start = now;
                }
                match dur_ns {
                    Some(d) if now >= self.stall_start + d => self.stall_done = true,
                    _ => return Err(io::ErrorKind::WouldBlock.into()),
                }
            }
            #[expect(
                clippy::panic,
                reason = "a fault the simulation injects; the scan must survive it"
            )]
            Fault::PanicAtBlock(b) if b == block_idx => panic!("injected panic"),
            _ => {}
        }
        let len = self.children.len();
        let limit = match n.fault {
            Fault::EioAfter(k) => k.min(len),
            _ => len,
        };
        if self.pos >= limit && limit < len {
            return Err(io::Error::from_raw_os_error(5));
        }
        let end = (self.pos + max_entries).min(limit);
        let raw = SimRaw {
            spec: self.spec.clone(),
            clock: self.clock.clone(),
            node: self.node,
            entries: self.children[self.pos..end].to_vec(),
            block: block_idx,
            stall_start: 0,
            stall_done: false,
        };
        self.pos = end;
        if self.pos >= limit && limit < len {
            self.eio_pending = true;
            return Ok((Box::new(raw), true));
        }
        Ok((Box::new(raw), self.pos < len))
    }
}

/// A block of a simulated directory, with the directory's stat-phase faults.
struct SimRaw {
    spec: Arc<Spec>,
    clock: Arc<SimClock>,
    node: u32,
    entries: Vec<u32>,
    block: usize,
    stall_start: u64,
    stall_done: bool,
}

impl RawBlock<u32> for SimRaw {
    fn len(&self) -> usize {
        self.entries.len()
    }

    fn stat(&mut self, out: &mut Block<u32>) -> io::Result<()> {
        match self.spec.nodes[self.node as usize].fault {
            Fault::StatStall { block, dur_ns } if block == self.block && !self.stall_done => {
                let now = self.clock.now();
                if self.stall_start == 0 {
                    self.stall_start = now;
                }
                match dur_ns {
                    Some(d) if now >= self.stall_start + d => self.stall_done = true,
                    _ => return Err(io::ErrorKind::WouldBlock.into()),
                }
            }
            #[expect(
                clippy::panic,
                reason = "a fault the simulation injects; the scan must survive it"
            )]
            Fault::StatPanic(b) if b == self.block => panic!("injected panic"),
            _ => {}
        }
        for &c in &self.entries {
            let cn = &self.spec.nodes[c as usize];
            let meta = if cn.vanished {
                Err(ErrKind::NotFound)
            } else {
                Ok(self.spec.meta(c))
            };
            let child = (cn.meta.kind == Kind::Dir).then_some(c);
            out.push(&cn.name, meta, child);
        }
        self.entries.clear();
        Ok(())
    }
}

impl Backend for SimBackend {
    type Loc = u32;
    type Stream = SimStream;

    #[expect(
        clippy::expect_used,
        reason = "the engine asks only for paths below the root, which the spec names"
    )]
    fn loc_from_path(&self, path: &Path) -> u32 {
        // Paths below the root are "/sim/d0/d1"; resolve them by name.
        let s = path.to_string_lossy();
        let rest = s.strip_prefix(ROOT_NAME).unwrap_or("");
        let mut cur = ROOT;
        for part in rest.split(['/', '\\']).filter(|p| !p.is_empty()) {
            let n = &self.spec.nodes[cur as usize];
            // A name can be both a removed entry and the one that replaced it.
            let named = |&&c: &&u32| self.spec.nodes[c as usize].name == part.as_bytes();
            cur = *n
                .children
                .iter()
                .filter(named)
                .find(|&&c| !self.spec.is_deleted(c))
                .or_else(|| n.children.iter().find(named))
                .expect("path in spec");
        }
        cur
    }

    fn stat_root(&self, loc: &u32) -> io::Result<Meta> {
        Ok(self.spec.meta(*loc))
    }

    fn read_prefix(&self, dir: &u32, name: &[u8], len: usize) -> Option<Vec<u8>> {
        let c = self
            .spec
            .live_children(*dir)
            .find(|&c| self.spec.nodes[c as usize].name == name)?;
        let n = &self.spec.nodes[c as usize];
        (n.meta.kind == Kind::File && !n.vanished)
            .then(|| n.content[..len.min(n.content.len())].to_vec())
    }

    fn mount_class(&self, mount: u64, dev: u64) -> Option<FsClass> {
        let n = self.spec.nodes.iter().find(|n| {
            n.meta.kind == Kind::Dir
                && if mount != 0 {
                    n.meta.mount == mount
                } else {
                    n.meta.dev == dev
                }
        })?;
        Some(n.fs_class.unwrap_or(FsClass::Local))
    }

    fn mount_storage(&self, mount: u64, dev: u64) -> Storage {
        self.spec
            .nodes
            .iter()
            .find(|n| n.meta.kind == Kind::Dir && mount_key(&n.meta) == mount_key_of(mount, dev))
            .map_or(Storage::Unknown, |n| n.storage)
    }

    fn duplicate_mounts(&self, _scope: &Path) -> Option<HashSet<u64>> {
        let mut dups = HashSet::new();
        for n in &self.spec.nodes {
            if n.dup_mount {
                if n.meta.mount == 0 {
                    // No mount ids: no mount table either.
                    return None;
                }
                dups.insert(n.meta.mount);
            }
        }
        Some(dups)
    }

    fn shares_extents(&self, loc: &u32) -> bool {
        let dev = self.spec.nodes[*loc as usize].meta.dev;
        self.spec.dev_fs.get(&dev).is_some_and(|d| d.1)
    }

    fn subvolume(&self, loc: &u32) -> io::Result<Option<Subvol>> {
        Ok(self.spec.nodes[*loc as usize].subvol.clone())
    }

    fn open(&self, loc: &u32, storage: Storage) -> io::Result<SimStream> {
        if storage != self.spec.nodes[*loc as usize].storage {
            self.wrong_storage.fetch_add(1, SeqCst);
        }
        if self.spec.is_deleted(*loc) {
            return Err(io::ErrorKind::NotFound.into());
        }
        match self.spec.nodes[*loc as usize].fault {
            Fault::Eacces => return Err(io::ErrorKind::PermissionDenied.into()),
            #[expect(
                clippy::panic,
                reason = "a fault the simulation injects; the scan must survive it"
            )]
            Fault::PanicOnOpen => panic!("injected panic"),
            _ => {}
        }
        Ok(SimStream {
            spec: self.spec.clone(),
            clock: self.clock.clone(),
            node: *loc,
            children: self.spec.live_children(*loc).collect(),
            pos: 0,
            stall_start: 0,
            stall_done: false,
            eio_pending: false,
        })
    }
}

impl SimBackend {
    /// What the simulated measurer answers: the spec's extent map, unless the file is gone or
    /// changed, or cannot be measured.
    pub fn measure(&self, req: &MeasureReq) -> MeasureMsg {
        let file = &req.files[0];
        let i = self.loc_from_path(&req.path(file));
        let n = &self.spec.nodes[i as usize];
        let result = if self.spec.is_deleted(i)
            || n.vanished
            || (n.meta.dev, n.meta.ino) != file.inode
            || n.meta.kind != Kind::File
        {
            MeasureResult::Changed
        } else if n.unmeasurable {
            MeasureResult::Failed(ErrKind::PermissionDenied)
        } else {
            let fs = self.spec.dev_fs[&n.meta.dev].0;
            let mut m = Measurement {
                fs: [(fs & 0xff) as u8; 16],
                ..Default::default()
            };
            for p in &n.pieces {
                m.compressed |= p.encoded;
                match (p.shared, p.encoded) {
                    (false, _) => m.excl += p.len,
                    (true, true) => m.points.push((p.phys, p.len)),
                    (true, false) => m.ranges.push((p.phys, p.phys + p.len)),
                }
            }
            MeasureResult::Measured(m)
        };
        MeasureMsg {
            round: req.round,
            node: file.node,
            inode: file.inode,
            result,
        }
    }
}

pub struct SimSink {
    pub(crate) q: Mutex<VecDeque<Batch>>,
    pub budget: Budget,
    pub max_used: AtomicU64,
}

impl Sink for SimSink {
    fn try_send(&self, mut b: Batch) -> Result<(), Batch> {
        match self.budget.try_acquire(b.bytes) {
            Some(n) => {
                b.bytes = n;
                self.q.lock().push_back(b);
                self.max_used.fetch_max(self.budget.used() as u64, SeqCst);
                Ok(())
            }
            None => Err(b),
        }
    }
    fn send(&self, _b: Batch) {
        unreachable!("simulated workers never block");
    }
    fn send_control(&self, m: ScanMsg) {
        self.q.lock().push_back(Batch {
            msgs: vec![m],
            bytes: 0,
        });
    }
}
