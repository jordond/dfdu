//! Tree mode's rows as the read builds them: folding, `*`, the row budget and a rescan swap
//! (`model::snapshot::tree`, `state::tree`).

use std::fs;
use std::time::{Duration, Instant};

use dfdu_engine::{Command, Engine, EngineOptions, Model, SizeMode};
use dfdu_scan::{JobOptions, PortableBackend};
use dfdu_tree::filetype::TypeFilter;
use dfdu_tree::{DirState, ErrKind, HostPath, Ix, Kind};

use super::meta;
use crate::app::App;
use crate::model::snapshot::{self, EntryData, Mark, Snapshot, Target};
use crate::state::order::{SortSpec, tree_order};
use crate::state::tree::{MAX_CHAIN, MAX_TREE_ROWS, TreeState};
use crate::theme::Theme;

mod keys;
mod props;
mod touches;

/// A finished scan of `/r` built by hand.
pub(crate) struct Fx {
    pub(crate) m: Model,
    pub(crate) root: Ix,
    ino: u64,
}

impl Fx {
    pub(crate) fn new() -> Self {
        Fx::at("/r")
    }

    /// A finished scan of `root`, empty.
    pub(crate) fn at(root: &str) -> Self {
        let mut m = Model::default();
        let root = m
            .tree
            .new_root(root.as_bytes(), &meta(Kind::Dir, 0, 1), true);
        m.tree.dir_mut(root).expect("root").state = DirState::Complete;
        Fx { m, root, ino: 1 }
    }

    pub(crate) fn dir(&mut self, parent: Ix, name: &str) -> Ix {
        self.ino += 1;
        let d = self
            .m
            .tree
            .add_dir(parent, name.as_bytes(), &meta(Kind::Dir, 0, self.ino));
        let own = self.m.tree.own(d);
        self.m.tree.propagate_add(d, &own);
        self.m.tree.dir_mut(d).expect("directory").state = DirState::Complete;
        d
    }

    pub(crate) fn file(&mut self, parent: Ix, name: &str, size: u64) -> Ix {
        self.ino += 1;
        let f = self.m.tree.add_file(
            parent,
            name.as_bytes(),
            &meta(Kind::File, size, self.ino),
            false,
            None,
        );
        let own = self.m.tree.own(f);
        self.m.tree.propagate_add(parent, &own);
        f
    }

    pub(crate) fn expand(&self, t: &mut TreeState, ix: Ix) {
        t.expand(Target::of(&self.m.tree, ix));
    }
}

/// A read for `app` as the run loop makes it: of its directory, in tree mode while the rows
/// are a tree, folded into it.
pub(crate) fn sync(app: &mut App, m: &Model, now: Instant) {
    let mut target = app.target.clone();
    let snap = snapshot::build_view(
        m,
        &mut target,
        app.size_mode,
        &app.types,
        false,
        app.tree_shown(),
    );
    app.target = target;
    app.refresh(snap, now);
}

/// A read of `/r` in tree mode, as the run loop makes it.
pub(crate) fn read(m: &Model, tree: &TreeState) -> Snapshot {
    snapshot::build_view(
        m,
        &mut Target::root(),
        SizeMode::Disk,
        &TypeFilter::default(),
        false,
        Some(tree),
    )
}

fn named<'a>(snap: &'a Snapshot, name: &str) -> &'a EntryData {
    snap.entries
        .iter()
        .find(|e| e.name[..] == *name.as_bytes())
        .unwrap_or_else(|| panic!("{name} missing"))
}

fn index_of(snap: &Snapshot, name: &str) -> usize {
    snap.entries
        .iter()
        .position(|e| e.name[..] == *name.as_bytes())
        .unwrap_or_else(|| panic!("{name} missing"))
}

fn children_of(snap: &Snapshot, parent: usize) -> Vec<&EntryData> {
    snap.entries
        .iter()
        .filter(|e| e.parent as usize == parent && e.parent != u32::MAX)
        .collect()
}

/// `/r` with `big/` holding files of 500, 300, 150, 4, 3, 2 and 1 bytes (the 1-byte one
/// unreadable), and a file `top` of 40. Returns `big` and its files.
fn folding() -> (Fx, Ix, Vec<Ix>) {
    let mut fx = Fx::new();
    let big = fx.dir(fx.root, "big");
    let mut files = Vec::new();
    for (i, size) in [500, 300, 150, 4, 3, 2, 1].into_iter().enumerate() {
        let f = fx.file(big, &format!("f{i}"), size);
        if size == 1 {
            fx.m.tree.set_error(f, ErrKind::PermissionDenied);
        }
        files.push(f);
    }
    fx.file(fx.root, "top", 40);
    (fx, big, files)
}

#[test]
fn tree_mode_off_reads_the_directory_only() {
    let (fx, ..) = folding();
    let snap = snapshot::build(&fx.m, &mut Target::root(), SizeMode::Disk);
    assert!(snap.tree.is_none());
    assert_eq!(snap.entries.len(), 2);
    assert!(
        snap.entries
            .iter()
            .all(|e| e.depth == 0 && e.parent == u32::MAX && e.fold == 0)
    );
}

#[test]
fn small_children_fold_and_add_up_to_the_parent() {
    let (fx, big, files) = folding();
    let mut t = TreeState::default();
    fx.expand(&mut t, big);
    let snap = read(&fx.m, &t);
    let b = index_of(&snap, "big");
    let kids = children_of(&snap, b);
    assert_eq!(kids.len(), 4, "three rows and the fold row");
    // 1% of 960 bytes is 9.6: the four children under it fold.
    let fold = kids.iter().find(|e| e.fold > 0).expect("a fold row");
    assert_eq!(fold.fold, 4);
    assert_eq!(fold.size, 10);
    assert!(fold.pseudo && !fold.is_dir);
    assert_eq!(fold.mark, Mark::Error, "the most severe mark among them");
    assert_eq!(fold.errors_inside, 1);
    assert_eq!((fold.depth, fold.parent as usize), (1, b));
    assert_eq!(
        fold.node,
        fx.m.tree.node_ref(files[3]),
        "the largest folded child's"
    );
    let sum: u64 = kids.iter().map(|e| e.size).sum();
    assert_eq!(sum, snap.entries[b].size);
    // The fold row comes last among its siblings, whatever the sort.
    for spec in [
        SortSpec::default(),
        SortSpec::for_key(crate::state::order::SortKey::Name),
    ] {
        let rows = tree_order(&snap.entries, spec);
        let at = rows
            .iter()
            .position(|&i| snap.entries[i].fold > 0)
            .expect("shown");
        assert_eq!(snap.entries[rows[at - 1]].parent as usize, b);
        assert_eq!(snap.entries[rows[at + 1]].name, b"top");
    }
}

#[test]
fn a_fold_row_expanded_in_place_shows_its_children() {
    let (mut fx, big, _) = folding();
    let mut t = TreeState::default();
    let other = fx.dir(fx.root, "other");
    for (i, size) in [900, 1].into_iter().enumerate() {
        fx.file(other, &format!("o{i}"), size);
    }
    fx.expand(&mut t, big);
    fx.expand(&mut t, other);
    t.unfold(fx.m.tree.node_ref(big));
    let snap = read(&fx.m, &t);
    let b = index_of(&snap, "big");
    let kids = children_of(&snap, b);
    assert_eq!(kids.len(), 7);
    assert!(kids.iter().all(|e| e.fold == 0));
    let o = index_of(&snap, "other");
    assert!(
        children_of(&snap, o).iter().any(|e| e.fold == 1),
        "the other directory still folds"
    );
}

#[test]
fn star_stops_below_the_chain_ratio() {
    // a/ (1000) holds b/ (900) and a file; b/ holds c/ (80) and 82 files of 10: c/ is the
    // largest child of b/ but under 10% of it.
    let mut fx = Fx::new();
    let a = fx.dir(fx.root, "a");
    fx.file(a, "fa", 100);
    let b = fx.dir(a, "b");
    let c = fx.dir(b, "c");
    fx.file(c, "fc", 80);
    for i in 0..82 {
        fx.file(b, &format!("fb{i}"), 10);
    }
    let mut t = TreeState::default();
    t.expand_chain(Target::of(&fx.m.tree, a));
    let snap = read(&fx.m, &t);
    let chained = snap
        .tree
        .as_ref()
        .and_then(|f| f.chained.clone())
        .expect("chained");
    let nodes: Vec<_> = chained.iter().map(|t| t.node).collect();
    assert_eq!(
        nodes,
        [Some(fx.m.tree.node_ref(a)), Some(fx.m.tree.node_ref(b))]
    );
    // The read already shows them, and they stay expanded.
    assert_eq!(named(&snap, "b").depth, 1);
    assert!(snap.entries.iter().any(|e| e.name == b"c" && e.depth == 2));
    let (ra, rb) = (fx.m.tree.node_ref(a), fx.m.tree.node_ref(b));
    t.apply(snap.tree.as_ref().expect("found"));
    assert!(t.chain.is_none());
    assert!(t.is_expanded(ra) && t.is_expanded(rb));
    assert!(!t.is_expanded(fx.m.tree.node_ref(c)));

    // With a lower ratio the chain goes on into c/, and ends at its file.
    let mut t = TreeState {
        chain_ratio: 0.05,
        ..TreeState::default()
    };
    t.expand_chain(Target::of(&fx.m.tree, a));
    let snap = read(&fx.m, &t);
    let chained = snap.tree.and_then(|f| f.chained).expect("chained");
    assert_eq!(chained.len(), 3);
}

#[test]
fn star_expands_at_most_eight_directories() {
    let mut fx = Fx::new();
    let mut cur = fx.root;
    let mut first = None;
    for i in 0..12 {
        cur = fx.dir(cur, &format!("d{i}"));
        first.get_or_insert(cur);
    }
    fx.file(cur, "f", 1000);
    let mut t = TreeState::default();
    t.expand_chain(Target::of(&fx.m.tree, first.expect("d0")));
    let snap = read(&fx.m, &t);
    let chained = snap
        .tree
        .as_ref()
        .and_then(|f| f.chained.clone())
        .expect("chained");
    assert_eq!(chained.len(), MAX_CHAIN);
    let deepest = snap.entries.iter().map(|e| e.depth).max();
    assert_eq!(deepest, Some(u8::try_from(MAX_CHAIN).expect("small")));
}

#[test]
fn a_huge_directory_gives_its_largest_children_up_to_the_budget() {
    let mut fx = Fx::new();
    let huge = fx.dir(fx.root, "huge");
    let n = MAX_TREE_ROWS + 10;
    for i in 0..n {
        fx.file(huge, &format!("f{i}"), 1000 + i as u64);
    }
    let mut t = TreeState::default();
    t.cycle_fold();
    t.cycle_fold();
    fx.expand(&mut t, huge);
    let snap = read(&fx.m, &t);
    assert!(snap.tree.as_ref().is_some_and(|f| f.truncated));
    let kids = children_of(&snap, index_of(&snap, "huge"));
    assert_eq!(kids.len(), MAX_TREE_ROWS + 1);
    let fold = kids.iter().find(|e| e.fold > 0).expect("fold row");
    assert_eq!(fold.fold, 10);
    assert_eq!(fold.size, (1000..1010).sum::<u64>(), "the ten smallest");
    let smallest = kids.iter().filter(|e| e.fold == 0).map(|e| e.size).min();
    assert_eq!(smallest, Some(1010));
}

#[test]
fn expanded_directories_survive_a_rescan_swap() {
    let tmp = tempfile::tempdir_in(dunce::canonicalize(std::env::temp_dir()).expect("temp"))
        .expect("tempdir");
    let dir = tmp.path().to_path_buf();
    fs::create_dir_all(dir.join("sub/inner")).expect("mkdir");
    fs::write(dir.join("sub/inner/f"), vec![0u8; 1000]).expect("write");
    fs::write(dir.join("sub/g"), vec![0u8; 1000]).expect("write");

    let engine = Engine::start(
        PortableBackend::default(),
        EngineOptions {
            workers: 2,
            ..Default::default()
        },
    );
    engine.send(Command::Scan {
        path: dir.clone().into(),
        opts: JobOptions::default(),
    });
    assert!(engine.wait_idle(Some(Duration::from_secs(20))).is_idle());
    let mut t = TreeState::default();
    let sub = engine.read(|m| {
        let sub = snapshot::find_path(&m.tree, &HostPath::local(&dir.join("sub"))).expect("sub");
        Target::of(&m.tree, sub)
    });
    let old = sub.node.expect("node");
    t.expand(sub);
    engine.send(Command::Rescan { node: old });
    assert!(engine.wait_idle(Some(Duration::from_secs(20))).is_idle());
    let snap = engine.read(|m| read(m, &t));
    engine.shutdown(Duration::from_millis(200));

    let found = snap.tree.as_ref().expect("found");
    assert!(found.gone.is_empty());
    let new = named(&snap, "sub").node;
    assert_ne!(new, old, "the rescan replaced the node");
    assert_eq!(found.moved, [(old, new)]);
    let kids = children_of(&snap, index_of(&snap, "sub"));
    assert!(kids.iter().any(|e| e.name == b"inner"), "still expanded");
    t.apply(found);
    assert!(t.is_expanded(new) && !t.is_expanded(old));
}

#[test]
fn expanding_sorts_the_new_children_at_once() {
    let (fx, big, _) = folding();
    let t0 = Instant::now();
    let mut app = App::new(Theme::no_color(), t0);
    app.tree = Some(TreeState::default());
    let sync = |app: &mut App| {
        let snap = read(&fx.m, app.tree.as_ref().expect("tree"));
        app.refresh(snap, t0);
    };
    sync(&mut app);
    assert_eq!(super::row_names(&app), ["big", "top"]);
    app.move_to(1);
    fx.expand(app.tree.as_mut().expect("tree"), big);
    sync(&mut app);
    assert_eq!(
        super::row_names(&app),
        ["big", "f0", "f1", "f2", "… 4 more", "top"],
        "no automatic sort is due, but a group seen for the first time is sorted"
    );
    assert_eq!(app.selected_entry().map(|e| &e.name[..]), Some(&b"top"[..]));
}

#[test]
fn a_read_before_the_scan_root_exists_is_not_stale() {
    let t = TreeState::default();
    let snap = read(&Model::default(), &t);
    assert!(!crate::state::tree::stale(Some(&t), &snap));
}
