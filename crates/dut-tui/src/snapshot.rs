//! Copying what one frame needs out of the engine model. This runs under the read lock, so it
//! only reads and copies; drawing happens after the lock is released (`data-pipeline.md` §3).

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use dut_engine::{DeleteSummary, JobKind, JobPhase, Model};
use dut_tree::{DirState, ErrKind, Ix, Kind, NodeRef, Totals, Tree, name_bytes};

/// Which size the list shows.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub enum SizeMode {
    /// Disk usage, data shared between files counted once (`docs/btrfs.md` §3.4).
    #[default]
    Disk,
    Apparent,
    /// Disk usage as `du` counts it: shared data once per file.
    Referenced,
}

impl SizeMode {
    pub fn of(self, t: &Totals) -> u64 {
        match self {
            SizeMode::Disk => t.alloc,
            SizeMode::Apparent => t.apparent,
            SizeMode::Referenced => t.referenced,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            SizeMode::Disk => "disk usage",
            SizeMode::Apparent => "apparent size",
            SizeMode::Referenced => "disk usage like du",
        }
    }
}

/// How an entry is marked in the list.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Mark {
    None,
    /// A directory still being scanned.
    Scanning,
    /// A directory whose scan is not responding.
    Stalled,
    /// A directory being rescanned.
    Rescanning,
    /// The entry or directory has an error, or its scan ended early.
    Error,
    /// A directory on another filesystem that was not scanned.
    OtherFs,
    /// A second mount of something the scan counts elsewhere; not scanned.
    DupMount,
    /// In a delete plan that has not finished.
    Deleting,
    /// Its scan was stopped (cancelled, or cut for a rescan or delete that did not complete):
    /// the size shown is a lower bound. Rescan with `r`.
    Incomplete,
}

#[derive(Clone, Debug)]
pub struct EntryData {
    pub node: NodeRef,
    pub name: Vec<u8>,
    pub kind: Kind,
    pub is_dir: bool,
    /// Size in the current mode (disk usage or apparent).
    pub size: u64,
    pub items: u64,
    pub mtime: i64,
    pub dev_ino: (u64, u64),
    pub mark: Mark,
    pub error: Option<ErrKind>,
    /// btrfs subvolume label ([`subvol_label`]).
    pub label: Option<String>,
    /// Bytes of its disk usage that data outside the scan also uses: deleting it frees less.
    pub shared_out: u64,
    /// A file with compressed extents whose on-disk size is not known (an upper bound).
    pub compressed: bool,
    /// The "shared between entries" row: data counted at the directory itself because entries
    /// share it. Not an entry; it cannot be opened or deleted.
    pub pseudo: bool,
}

impl EntryData {
    pub fn hidden(&self) -> bool {
        self.name.first() == Some(&b'.')
    }
}

/// Identity of a node, compared when a reference is re-resolved by path after a rescan swap.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct Ident {
    pub dev_ino: (u64, u64),
    pub kind: Kind,
}

/// A node the UI refers to: the reference, plus the path and identity to find it again.
#[derive(Clone, Debug)]
pub struct Target {
    pub node: Option<NodeRef>,
    pub path: PathBuf,
    pub ident: Option<Ident>,
}

impl Target {
    pub fn root() -> Self {
        Target {
            node: None,
            path: PathBuf::new(),
            ident: None,
        }
    }

    pub fn of(tree: &Tree, ix: Ix) -> Self {
        Target {
            node: Some(tree.node_ref(ix)),
            path: tree.path_buf(ix),
            ident: Some(ident(tree, ix)),
        }
    }
}

pub fn ident(tree: &Tree, ix: Ix) -> Ident {
    Ident {
        dev_ino: tree.dev_ino(ix),
        kind: tree.kind(ix),
    }
}

/// Finds a visible node by path: the root whose path is a prefix of it, then one child per
/// remaining component.
pub fn find_path(tree: &Tree, path: &Path) -> Option<Ix> {
    for root in tree.roots() {
        let Ok(rest) = path.strip_prefix(tree.path_buf(root)) else {
            continue;
        };
        let mut cur = root;
        for part in rest.components() {
            let name = name_bytes(part.as_os_str());
            cur = tree.children(cur).find(|&c| *tree.name(c) == *name)?;
        }
        return Some(cur);
    }
    None
}

/// Resolves a target: by reference if it still resolves, otherwise by path with an identity
/// check. Returns `None` if neither works.
pub fn resolve(tree: &Tree, t: &Target) -> Option<Ix> {
    if let Some(ix) = t.node.and_then(|r| tree.resolve_visible(r)) {
        return Some(ix);
    }
    let ix = find_path(tree, &t.path)?;
    match t.ident {
        Some(id) if id.dev_ino != (0, 0) && ident(tree, ix) != id => None,
        _ => Some(ix),
    }
}

#[derive(Clone, Debug, Default)]
pub struct Snapshot {
    pub dir_path: PathBuf,
    pub dir_node: Option<NodeRef>,
    pub at_root: bool,
    pub dir_total: Totals,
    pub dir_mark: Option<Mark>,
    pub root_total: Totals,
    pub entries: Vec<EntryData>,
    pub busy: bool,
    pub stalled: usize,
    pub incomplete: bool,
    pub notice: Option<String>,
    /// `(done, total)` of the running delete plan.
    pub deleting: Option<(usize, usize)>,
    pub last_delete: Option<DeleteSummary>,
    /// `(done, requested)` of the shared-extent measurement in progress.
    pub measuring: Option<(u64, u64)>,
    /// Files whose extents could not be measured.
    pub unmeasured: u64,
    /// Compressed files are shown uncompressed (an upper bound); exact as root.
    pub compressed_upper_bound: bool,
    /// btrfs data profile and raw bytes per byte of data.
    pub profile: Option<(&'static str, f64)>,
    /// `(job, dir_id)` of the current directory, for scan prioritisation.
    pub focus: Option<(u32, u32)>,
    /// Cheap fingerprint of the model, to skip redraws when nothing changed.
    pub version: u64,
}

/// A fingerprint that changes whenever anything visible could have changed.
pub fn version(m: &Model) -> u64 {
    m.fingerprint()
}

/// "subvolume" or "snapshot of NAME", with ", read-only", for btrfs subvolume roots.
pub fn subvol_label(m: &Model, ix: Ix) -> Option<String> {
    let tree = &m.tree;
    let sv = m.subvols.get(&tree.node_ref(ix))?;
    let mut s = match sv.parent_uuid {
        Some(origin) => match m
            .subvols
            .iter()
            .find(|(_, o)| o.uuid == origin)
            .and_then(|(r, _)| tree.resolve_visible(*r))
        {
            Some(o) => format!("snapshot of {}", String::from_utf8_lossy(tree.name(o))),
            None => "snapshot".into(),
        },
        None if sv.id == 5 => "btrfs top level".into(),
        None => "subvolume".into(),
    };
    if sv.read_only {
        s.push_str(", read-only");
    }
    if let Some((rfer, excl)) = sv.qgroup {
        s.push_str(&format!(
            "; quota: {} referenced, {} exclusive",
            crate::theme::human(rfer),
            crate::theme::human(excl)
        ));
    }
    Some(s)
}

fn mark_of(tree: &Tree, ix: Ix, rescanning: &HashSet<NodeRef>) -> Mark {
    if tree.flags(ix).has(dut_tree::NodeFlags::DELETING) {
        return Mark::Deleting;
    }
    if tree.flags(ix).has(dut_tree::NodeFlags::DUP_MOUNT) {
        return Mark::DupMount;
    }
    if rescanning.contains(&tree.node_ref(ix)) {
        return Mark::Rescanning;
    }
    match tree.dir(ix) {
        Some(d) => match d.state {
            DirState::Pending if d.stalled => Mark::Stalled,
            DirState::Pending => Mark::Scanning,
            DirState::Skipped => Mark::OtherFs,
            DirState::Failed | DirState::Partial | DirState::Abandoned | DirState::Lost => {
                Mark::Error
            }
            DirState::Cancelled if tree.total(ix).items <= 1 => Mark::Error,
            _ if d.cut || matches!(d.state, DirState::Cancelled | DirState::Superseded) => {
                Mark::Incomplete
            }
            _ if tree.error(ix).is_some() => Mark::Error,
            _ => Mark::None,
        },
        None if tree.error(ix).is_some() => Mark::Error,
        None => Mark::None,
    }
}

/// Builds the snapshot for the directory `target` points at, updating `target` if the node had
/// to be found again (rescan swap) or no longer exists (falls back to the nearest ancestor).
pub fn build(m: &Model, target: &mut Target, mode: SizeMode) -> Snapshot {
    let tree = &m.tree;
    let size = |t: &Totals| mode.of(t);
    let mut snap = Snapshot {
        busy: m.status.busy(),
        stalled: m.status.stalled.len(),
        incomplete: m.status.incomplete,
        notice: m.status.notices.last().cloned(),
        deleting: m.status.deleting.as_ref().map(|d| (d.done, d.total)),
        last_delete: m.status.last_delete.clone(),
        measuring: m.status.measuring.as_ref().map(|p| (p.done, p.requested)),
        unmeasured: m.status.unmeasured,
        compressed_upper_bound: m.status.compressed_upper_bound,
        profile: m.status.profile,
        version: version(m),
        ..Default::default()
    };
    let Some(root) = tree.roots().next() else {
        return snap;
    };
    snap.root_total = tree.total(root);

    let mut dir = resolve(tree, target);
    // The directory is gone: go up until something resolves.
    let mut path = target.path.clone();
    while dir.is_none() && path.pop() {
        dir = find_path(tree, &path);
    }
    let dir = dir.filter(|&d| tree.dir(d).is_some()).unwrap_or(root);
    *target = Target::of(tree, dir);

    let rescanning: HashSet<NodeRef> = m
        .status
        .jobs
        .iter()
        .filter(|j| {
            j.kind == JobKind::Rescan && matches!(j.phase, JobPhase::Running | JobPhase::Finishing)
        })
        .map(|j| j.target)
        .collect();

    let rec = tree.dir(dir).expect("directory");
    snap.focus = Some((rec.job, rec.dir_id));
    snap.dir_path = tree.path_buf(dir);
    snap.dir_node = Some(tree.node_ref(dir));
    snap.at_root = dir == root;
    snap.dir_total = tree.total(dir);
    snap.dir_mark = Some(mark_of(tree, dir, &rescanning));
    snap.entries = tree
        .children(dir)
        .map(|c| {
            let t = tree.total(c);
            EntryData {
                node: tree.node_ref(c),
                name: tree.name(c).to_vec(),
                kind: tree.kind(c),
                is_dir: tree.dir(c).is_some(),
                size: size(&t),
                items: t.items,
                mtime: tree.mtime(c),
                dev_ino: tree.dev_ino(c),
                mark: mark_of(tree, c, &rescanning),
                error: tree.error(c),
                label: subvol_label(m, c),
                shared_out: t.shared_out,
                compressed: tree.flags(c).has(dut_tree::NodeFlags::COMPRESSED),
                pseudo: false,
            }
        })
        .collect();
    // Data the entries share is counted at the directory: show it as its own row.
    let home = tree.home(dir);
    if mode == SizeMode::Disk && home > 0 {
        snap.entries.push(EntryData {
            node: tree.node_ref(dir),
            name: b"(shared between entries)".to_vec(),
            kind: Kind::Other,
            is_dir: false,
            size: home,
            items: 0,
            mtime: 0,
            dev_ino: (0, 0),
            mark: Mark::None,
            error: None,
            label: None,
            shared_out: 0,
            compressed: false,
            pseudo: true,
        });
    }
    snap
}

/// An entry with a unique node reference, for tests that do not need a real tree.
#[cfg(test)]
pub fn test_entry(i: u32, name: &str, size: u64) -> EntryData {
    use dut_tree::Meta;
    let mut t = Tree::new();
    let meta = Meta {
        kind: Kind::Dir,
        apparent: 0,
        alloc: 0,
        mtime: 0,
        dev: 1,
        ino: 1,
        nlink: 1,
        mount: 0,
    };
    let root = t.new_root(b"/", &meta, true);
    let file = Meta {
        kind: Kind::File,
        ..meta
    };
    let mut last = root;
    for _ in 0..=i {
        last = t.add_file(root, b"x", &file, false, None);
    }
    EntryData {
        node: t.node_ref(last),
        name: name.as_bytes().to_vec(),
        kind: Kind::File,
        is_dir: false,
        size,
        items: 1,
        mtime: 0,
        dev_ino: (1, i as u64 + 10),
        mark: Mark::None,
        error: None,
        label: None,
        shared_out: 0,
        compressed: false,
        pseudo: false,
    }
}
