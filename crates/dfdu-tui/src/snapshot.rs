//! Copying what one frame needs out of the engine model. This runs under the read lock, so it
//! only reads and copies; drawing happens after the lock is released.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use dfdu_engine::{DeleteSummary, JobKind, JobPhase, Model};
use dfdu_tree::filetype::{self, Breakdown, TypeFilter};
use dfdu_tree::{DirState, ErrKind, Ix, Kind, NodeRef, Totals, Tree, name_bytes};

/// Which size the list shows.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub enum SizeMode {
    /// Disk usage, data shared between files counted once.
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
    /// A directory read in full, with errors somewhere below it.
    ErrorsInside,
    /// A network filesystem, not scanned without `--network`.
    Network,
    /// A kernel filesystem (`/proc`, `/sys`), never scanned.
    Kernel,
    /// In a delete plan that has not finished.
    Deleting,
    /// Its scan was stopped (cancelled, or cut for a rescan or delete that did not complete):
    /// the size shown is a lower bound. Rescan with `r`.
    Incomplete,
}

/// An entry name. Short names (most of them) are stored inline, so that copying a directory
/// of a million entries into a snapshot, and dropping the previous one, does not allocate and
/// free a million times.
#[derive(Clone)]
pub enum Name {
    Inline(u8, [u8; Name::INLINE]),
    Heap(Box<[u8]>),
}

impl Name {
    const INLINE: usize = 22;
}

impl From<&[u8]> for Name {
    fn from(b: &[u8]) -> Self {
        if b.len() <= Name::INLINE {
            let mut buf = [0; Name::INLINE];
            buf[..b.len()].copy_from_slice(b);
            Name::Inline(b.len() as u8, buf)
        } else {
            Name::Heap(b.into())
        }
    }
}

impl std::ops::Deref for Name {
    type Target = [u8];
    fn deref(&self) -> &[u8] {
        match self {
            Name::Inline(n, buf) => &buf[..*n as usize],
            Name::Heap(b) => b,
        }
    }
}

impl std::fmt::Debug for Name {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        String::from_utf8_lossy(self).fmt(f)
    }
}

impl PartialEq for Name {
    fn eq(&self, other: &Name) -> bool {
        self[..] == other[..]
    }
}

impl Eq for Name {}

impl<const N: usize> PartialEq<&[u8; N]> for Name {
    fn eq(&self, other: &&[u8; N]) -> bool {
        self[..] == other[..]
    }
}

/// What a row counts as for cleanup: the class of the rule it matched, or of the match above
/// it that covers it (`Tree::class_in_effect`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Tag {
    pub class: dfdu_tree::Class,
    /// The match is above the row, not the row itself.
    pub inherited: bool,
}

impl Tag {
    /// The short name of its category (`build` for `dev.build`) and the theme's colour slot.
    pub fn category(self, rules: &dfdu_rules::RuleSet) -> (&'static str, usize) {
        let c = rules.rule(self.class.rule).category();
        (c.id.rsplit('.').next().unwrap_or(c.id), c.color as usize)
    }

    /// A match offered for cleanup without review: safe or regenerable (a match version
    /// control tracks is protected).
    pub fn candidate(self) -> bool {
        self.class.safety <= dfdu_rules::Safety::Regenerable
    }
}

/// The tag of node `c` of a directory whose own class in effect is `dir`. Without classes
/// this is one check of an empty map.
pub fn tag_of(tree: &Tree, dir: Option<dfdu_tree::Class>, c: Ix) -> Option<Tag> {
    match dir {
        Some(class) => Some(Tag {
            class,
            inherited: true,
        }),
        None => tree.class(c).map(|class| Tag {
            class,
            inherited: false,
        }),
    }
}

#[derive(Clone, Debug)]
pub struct EntryData {
    pub node: NodeRef,
    pub name: Name,
    pub kind: Kind,
    pub is_dir: bool,
    /// Size in the current [`SizeMode`].
    pub size: u64,
    pub items: u64,
    pub mtime: i64,
    pub dev_ino: (u64, u64),
    pub mark: Mark,
    pub error: Option<ErrKind>,
    /// What went wrong reading it ([`crate::problem`]).
    pub problem: Option<String>,
    /// Errors below it, not counting its own.
    pub errors_inside: u64,
    /// btrfs subvolume label ([`subvol_label`]).
    pub label: Option<String>,
    pub tag: Option<Tag>,
    /// Bytes of its disk usage that data outside the scan also uses: deleting it frees less.
    pub shared_out: u64,
    /// A file with compressed extents whose on-disk size is not known (an upper bound).
    pub compressed: bool,
    /// The "shared between entries" row: data counted at the directory itself because entries
    /// share it. Not an entry; it cannot be opened or deleted.
    pub pseudo: bool,
    /// A deleted entry kept in its row until the next re-sort, drawn struck through. Always
    /// also `pseudo` and never `is_dir`, so it cannot be opened or deleted either.
    pub ghost: bool,
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

    /// By reference if it still resolves, otherwise by path with an identity check. `None` if
    /// neither works.
    pub fn resolve(&self, tree: &Tree) -> Option<Ix> {
        if let Some(ix) = self.node.and_then(|r| tree.resolve_visible(r)) {
            return Some(ix);
        }
        let ix = find_path(tree, &self.path)?;
        self.matches(tree, ix).then_some(ix)
    }

    /// The identity check for a node found by path: `ix` has the target's kind and device and
    /// inode numbers, so a new file or directory under the old name is not taken for it. A
    /// target without an identity, or without device and inode numbers, passes.
    pub fn matches(&self, tree: &Tree, ix: Ix) -> bool {
        self.ident
            .is_none_or(|id| id.dev_ino == (0, 0) || ident(tree, ix) == id)
    }
}

fn ident(tree: &Tree, ix: Ix) -> Ident {
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

#[derive(Clone, Debug, Default)]
pub struct Snapshot {
    /// The rules the scan classifies against, when it does: rows have a category column.
    pub rules: Option<std::sync::Arc<dfdu_rules::RuleSet>>,
    pub dir_path: PathBuf,
    /// Path of the scan root; `dir_path` is below it or equal to it.
    pub root_path: PathBuf,
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
    /// What [`Command::Undo`](dfdu_engine::Command::Undo) would put back.
    pub undo: Option<dfdu_engine::UndoInfo>,
    pub undoing: bool,
    /// Items staged and not deleted yet, and their size.
    pub staged: dfdu_engine::Count,
    /// The latest estimate of what a delete would free.
    pub estimate: Option<dfdu_engine::Estimate>,
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
    /// The file type filter the entries were read with ([`crate::types`]); empty: none. With
    /// one, entries and `dir_total` count the matching files only.
    pub types: TypeFilter,
    /// The directory's files by type and extension, read while the type picker is open.
    pub breakdown: Option<std::sync::Arc<Breakdown>>,
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
    if tree.flags(ix).has(dfdu_tree::NodeFlags::DELETING) {
        return Mark::Deleting;
    }
    if tree.flags(ix).has(dfdu_tree::NodeFlags::DUP_MOUNT) {
        return Mark::DupMount;
    }
    if tree.flags(ix).has(dfdu_tree::NodeFlags::KERNEL) {
        return Mark::Kernel;
    }
    if tree.flags(ix).has(dfdu_tree::NodeFlags::NETWORK) {
        return Mark::Network;
    }
    if !rescanning.is_empty() && rescanning.contains(&tree.node_ref(ix)) {
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
            _ if tree.total(ix).errors > 0 => Mark::ErrorsInside,
            _ => Mark::None,
        },
        None if tree.error(ix).is_some() => Mark::Error,
        None => Mark::None,
    }
}

/// Builds the snapshot for the directory `target` points at, updating `target` if the node had
/// to be found again (rescan swap) or no longer exists (falls back to the nearest ancestor).
#[cfg(test)]
pub fn build(m: &Model, target: &mut Target, mode: SizeMode) -> Snapshot {
    build_view(m, target, mode, &TypeFilter::default(), false)
}

/// [`build`] counting only the files `types` matches (none left out when it is empty), and with
/// the directory's [`Breakdown`] when `breakdown` is set. Both walk the directory's subtree.
pub fn build_view(
    m: &Model,
    target: &mut Target,
    mode: SizeMode,
    types: &TypeFilter,
    breakdown: bool,
) -> Snapshot {
    let tree = &m.tree;
    let size = |t: &Totals| mode.of(t);
    let mut snap = Snapshot {
        busy: m.status.busy(),
        stalled: m.status.stalled.len(),
        incomplete: m.status.incomplete,
        notice: m.status.notices.last().cloned(),
        deleting: m.status.deleting.as_ref().map(|d| (d.done, d.total)),
        undo: m.status.undo.clone(),
        undoing: m.status.undoing.is_some(),
        staged: m.status.staged,
        estimate: m.status.estimate.clone(),
        last_delete: m.status.last_delete.clone(),
        measuring: m.status.measuring.as_ref().map(|p| (p.done, p.requested)),
        unmeasured: m.status.unmeasured,
        compressed_upper_bound: m.status.compressed_upper_bound,
        profile: m.status.profile,
        version: m.fingerprint(),
        rules: m.rules.clone(),
        types: types.clone(),
        ..Default::default()
    };
    let Some(root) = tree.roots().next() else {
        return snap;
    };
    snap.root_total = tree.total(root);

    let mut dir = target.resolve(tree);
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
    snap.root_path = tree.path_buf(root);
    snap.dir_node = Some(tree.node_ref(dir));
    snap.at_root = dir == root;
    snap.dir_total = tree.total(dir);
    snap.dir_mark = Some(mark_of(tree, dir, &rescanning));
    let dir_class = tree.class_in_effect(dir).map(|(_, c)| c);
    // This runs under the read lock for every entry of the directory, which can have a
    // million: each lookup is done once, and the error and subvolume lookups only when the
    // entry has an error or the scan has subvolumes.
    snap.entries = tree
        .children(dir)
        .map(|c| {
            let t = tree.total(c);
            let error = tree.error(c);
            EntryData {
                node: tree.node_ref(c),
                name: tree.name(c).into(),
                kind: tree.kind(c),
                is_dir: tree.dir(c).is_some(),
                size: size(&t),
                items: t.items,
                mtime: tree.mtime(c),
                dev_ino: tree.dev_ino(c),
                mark: mark_of(tree, c, &rescanning),
                error,
                problem: error.and_then(|_| crate::problem(tree, c)),
                errors_inside: t.errors - error.is_some() as u64,
                label: if m.subvols.is_empty() {
                    None
                } else {
                    subvol_label(m, c)
                },
                tag: tag_of(tree, dir_class, c),
                shared_out: t.shared_out,
                compressed: tree.flags(c).has(dfdu_tree::NodeFlags::COMPRESSED),
                pseudo: false,
                ghost: false,
            }
        })
        .collect();
    if breakdown {
        let mut b = Breakdown::default();
        filetype::sum(tree, dir, &TypeFilter::default(), Some(&mut b));
        snap.breakdown = Some(std::sync::Arc::new(b));
    }
    if !types.is_empty() {
        only_types(tree, dir, types, mode, &mut snap);
        return snap;
    }
    // Data the entries share is counted at the directory: show it as its own row.
    let home = tree.home(dir);
    if mode == SizeMode::Disk && home > 0 {
        snap.entries.push(EntryData {
            node: tree.node_ref(dir),
            name: b"(shared between entries)"[..].into(),
            kind: Kind::Other,
            is_dir: false,
            size: home,
            items: 0,
            mtime: 0,
            dev_ino: (0, 0),
            mark: Mark::None,
            error: None,
            problem: None,
            errors_inside: 0,
            label: None,
            tag: None,
            shared_out: 0,
            compressed: false,
            pseudo: true,
            ghost: false,
        });
    }
    snap
}

/// Keeps the entries with files `types` matches, sized by those files, and sets `dir_total` to
/// their sum (errors and the rest of the directory's own totals kept).
fn only_types(tree: &Tree, dir: Ix, types: &TypeFilter, mode: SizeMode, snap: &mut Snapshot) {
    let mut sum = Totals::default();
    snap.entries.retain_mut(|e| {
        let Some(c) = tree.resolve_visible(e.node) else {
            return false;
        };
        let t = filetype::sum(tree, c, types, None);
        if t.items == 0 {
            return false;
        }
        sum.add(&t);
        e.size = mode.of(&t);
        e.items = t.items;
        e.shared_out = t.shared_out;
        true
    });
    let d = tree.total(dir);
    snap.dir_total = Totals {
        errors: d.errors,
        mounts: d.mounts,
        ro_subvols: d.ro_subvols,
        ..sum
    };
}

/// An entry with a unique node reference, for tests that do not need a real tree.
#[cfg(test)]
pub fn test_entry(i: u32, name: &str, size: u64) -> EntryData {
    let mut t = Tree::new();
    let root = t.new_root(b"/", &crate::tests::meta(Kind::Dir, 0, 1), true);
    let file = crate::tests::meta(Kind::File, 0, 1);
    let mut last = root;
    for _ in 0..=i {
        last = t.add_file(root, b"x", &file, false, None);
    }
    EntryData {
        node: t.node_ref(last),
        name: name.as_bytes().into(),
        kind: Kind::File,
        is_dir: false,
        size,
        items: 1,
        mtime: 0,
        dev_ino: (1, i as u64 + 10),
        mark: Mark::None,
        error: None,
        problem: None,
        errors_inside: 0,
        label: None,
        tag: None,
        shared_out: 0,
        compressed: false,
        pseudo: false,
        ghost: false,
    }
}
