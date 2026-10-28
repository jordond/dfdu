//! Marked entries: `Space` marks, the footer shows their live total, and a delete acts on them.

use std::borrow::Cow;
use std::collections::{HashMap, HashSet};
use std::path::Path;

use dfdu_engine::{JobKind, JobPhase, Model};
use dfdu_tree::{DirState, Ix, NodeRef, name_bytes};
use ratatui::text::Span;

use crate::app::App;
use crate::snapshot::{EntryData, Ident, Mark, Target, find_path};
use crate::theme::human;

/// One marked entry: how to find it again, and its sizes as the last read found them.
#[derive(Clone, Debug)]
pub struct Item {
    /// The reference, with the path and identity that find it again after a rescan swap.
    pub target: Target,
    pub is_dir: bool,
    /// Size in the size mode of the last read.
    pub size: u64,
    pub items: u64,
    /// Part of `size` that data outside the scan also uses.
    pub shared_out: u64,
    /// A directory still being scanned: `size` is a lower bound.
    pub scanning: bool,
    /// Below another marked item: counted and deleted with that one.
    pub inside: bool,
}

/// The marked entries, in any directory, in the order they were marked.
#[derive(Clone, Debug, Default)]
pub struct Marks {
    items: Vec<Item>,
    /// The references in `items`, for drawing rows.
    nodes: HashSet<NodeRef>,
    /// Counts the changes made to `items` by the user.
    version: u64,
    /// The `version` the last [`apply`] took in.
    applied: u64,
}

impl Marks {
    /// The marked entries, in the order they were marked.
    pub fn items(&self) -> &[Item] {
        &self.items
    }

    /// True when `node` is marked.
    pub fn contains(&self, node: NodeRef) -> bool {
        self.nodes.contains(&node)
    }

    /// True when nothing is marked.
    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    /// The total of the outermost items, and whether any of them is still being scanned.
    pub fn total(&self) -> (u64, bool) {
        self.items
            .iter()
            .filter(|it| !it.inside)
            .fold((0, false), |(n, scanning), it| {
                (n + it.size, scanning || it.scanning)
            })
    }

    /// Records a change by the user: the sizes need a read.
    fn changed(&mut self) {
        self.version += 1;
        self.index();
    }

    /// Rebuilds `nodes` and which items are inside others: an item is inside when one of its
    /// ancestors is marked, so this costs a set lookup per mark and path component.
    fn index(&mut self) {
        self.nodes = self.items.iter().filter_map(|it| it.target.node).collect();
        let paths: HashSet<&Path> = self.items.iter().map(|it| &*it.target.path).collect();
        let inside: Vec<bool> = self
            .items
            .iter()
            .map(|it| {
                it.target
                    .path
                    .ancestors()
                    .skip(1)
                    .any(|a| paths.contains(a))
            })
            .collect();
        for (it, inside) in self.items.iter_mut().zip(inside) {
            it.inside = inside;
        }
    }
}

/// What [`read`] found for each mark, in the order of [`Marks::items`].
#[derive(Clone, Debug, Default)]
pub struct Marked {
    /// The `Marks::version` this was read for.
    version: u64,
    found: Vec<Option<Found>>,
}

#[derive(Clone, Debug)]
struct Found {
    node: NodeRef,
    size: u64,
    items: u64,
    shared_out: u64,
    scanning: bool,
}

/// Marks or unmarks the selected entry and moves the cursor down a row. The "shared between
/// entries" row and deleted rows are not entries and cannot be marked.
pub fn toggle(app: &mut App) {
    let Some(e) = app.selected_entry().cloned() else {
        return;
    };
    if e.pseudo {
        app.status = Some("only entries can be marked".into());
        return;
    }
    if e.is_dir && crate::types::active(app) {
        app.status = Some(crate::types::NO_FOLDERS.into());
        return;
    }
    let path = app.snap.dir_path.join(&*dfdu_tree::name_os(&e.name));
    let marks = &mut app.marks;
    match marks
        .items
        .iter()
        .position(|it| it.target.node == Some(e.node) || it.target.path == path)
    {
        Some(i) => {
            marks.items.remove(i);
        }
        None => marks.items.push(Item {
            target: Target {
                node: Some(e.node),
                path,
                ident: Some(Ident {
                    dev_ino: e.dev_ino,
                    kind: e.kind,
                }),
            },
            is_dir: e.is_dir,
            size: e.size,
            items: e.items,
            shared_out: e.shared_out,
            scanning: matches!(e.mark, Mark::Scanning | Mark::Stalled | Mark::Rescanning),
            inside: false,
        }),
    }
    marks.changed();
    app.move_by(1);
}

/// Marks `item` or unmarks it (found by its node or path), as the cleanup view does for whole
/// groups.
pub fn set(app: &mut App, item: Item, on: bool) {
    let marks = &mut app.marks;
    let at = marks
        .items
        .iter()
        .position(|it| it.target.node == item.target.node || it.target.path == item.target.path);
    match (at, on) {
        (None, true) => marks.items.push(item),
        (Some(i), false) => {
            marks.items.remove(i);
        }
        _ => return,
    }
    marks.changed();
}

pub fn clear(app: &mut App) {
    if !app.marks.is_empty() {
        app.marks.items.clear();
        app.marks.changed();
    }
}

/// The first cell of a row: `●` when the entry is marked.
pub fn gutter(app: &App, e: &EntryData) -> Span<'static> {
    if !e.pseudo && app.marks.contains(e.node) {
        Span::styled("●", app.theme.mark)
    } else {
        Span::raw(" ")
    }
}

/// How many entries are marked and their total, for the status line.
pub fn summary(app: &App) -> Option<Span<'static>> {
    let marks = &app.marks;
    if marks.is_empty() {
        return None;
    }
    let (total, scanning) = marks.total();
    let at_least = if scanning { "at least " } else { "" };
    Some(Span::styled(
        format!(
            "● {} marked ({at_least}{})",
            marks.items.len(),
            human(total)
        ),
        app.theme.mark,
    ))
}

/// True when the marks changed since [`read`] last ran. A read ([`read`], then [`apply`]) must
/// make it false: while it is true, the run loop reads again at once.
pub fn stale(app: &App) -> bool {
    app.marks.version != app.marks.applied
}

/// Resolves the marks in the model; runs under the read lock. Each mark costs a walk up from
/// its node. The marks whose reference no longer resolves (after a rescan swap) are found by
/// path, together per parent directory: a walk down to it and one pass over its children, so
/// the cost is bounded by the children of those directories, not by marks times children.
pub fn read(m: &Model, app: &App) -> Marked {
    let tree = &m.tree;
    let items = app.marks.items();
    let rescanning = |r: NodeRef| {
        m.status.jobs.iter().any(|j| {
            j.kind == JobKind::Rescan
                && j.target == r
                && matches!(j.phase, JobPhase::Running | JobPhase::Finishing)
        })
    };
    let mut ixs: Vec<Option<Ix>> = items
        .iter()
        .map(|it| it.target.node.and_then(|r| tree.resolve_visible(r)))
        .collect();
    let mut lost: HashMap<&Path, HashMap<Cow<'_, [u8]>, Vec<usize>>> = HashMap::new();
    for (i, it) in items.iter().enumerate() {
        if ixs[i].is_some() {
            continue;
        }
        let path = &it.target.path;
        match (path.parent(), path.file_name()) {
            (Some(dir), Some(name)) => lost
                .entry(dir)
                .or_default()
                .entry(name_bytes(name))
                .or_default()
                .push(i),
            _ => ixs[i] = it.target.resolve(tree),
        }
    }
    for (dir, mut names) in lost {
        let Some(d) = find_path(tree, dir) else {
            for i in names.into_values().flatten() {
                ixs[i] = items[i].target.resolve(tree);
            }
            continue;
        };
        for c in tree.children(d) {
            if names.is_empty() {
                break;
            }
            for i in names.remove(tree.name(c)).into_iter().flatten() {
                ixs[i] = items[i].target.matches(tree, c).then_some(c);
            }
        }
    }
    let found = ixs
        .into_iter()
        .map(|ix| {
            let ix = ix?;
            let node = tree.node_ref(ix);
            let t = tree.total(ix);
            Some(Found {
                node,
                size: app.size_mode.of(&t),
                items: t.items,
                shared_out: t.shared_out,
                scanning: tree.dir(ix).is_some_and(|d| d.state == DirState::Pending)
                    || rescanning(node),
            })
        })
        .collect();
    Marked {
        version: app.marks.version,
        found,
    }
}

/// Takes in what [`read`] found, now in `app.extras.marked`, after every read (with a new
/// snapshot or without): updates the sizes, follows rescan swaps and drops the marks that no
/// longer resolve (deleted, or gone after a rescan).
pub fn apply(app: &mut App) {
    let (marks, marked) = (&mut app.marks, &app.extras.marked);
    if marked.version != marks.version || marked.found.len() != marks.items.len() {
        return;
    }
    marks.applied = marks.version;
    let before = marks.items.len();
    let mut swapped = false;
    let mut found = marked.found.iter();
    marks.items.retain_mut(|it| {
        let Some(Some(f)) = found.next() else {
            return false;
        };
        swapped |= it.target.node != Some(f.node);
        it.target.node = Some(f.node);
        (it.size, it.items, it.shared_out, it.scanning) =
            (f.size, f.items, f.shared_out, f.scanning);
        true
    });
    if swapped || marks.items.len() != before {
        marks.index();
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::time::{Duration, Instant};

    use dfdu_engine::{Command, Engine, EngineOptions};
    use dfdu_scan::{JobOptions, PortableBackend};
    use dfdu_tree::{Kind, Meta, Totals};
    use ratatui::crossterm::event::KeyCode;
    use ratatui::style::Color;

    use super::*;
    use crate::app::Effect;
    use crate::app::tests::snap;
    use crate::snapshot::{self, Snapshot, test_entry};
    use crate::tests::{key, render, render_buffer, style_at, styled, text};
    use crate::theme::Theme;

    /// Reads the model as the run loop does for a frame with a new snapshot.
    fn sync(app: &mut App, m: &Model, now: Instant) {
        let mut target = app.target.clone();
        let snap = snapshot::build(m, &mut target, app.size_mode);
        let marked = read(m, app);
        app.target = target;
        app.refresh(snap, now);
        app.extras.marked = marked;
        apply(app);
    }

    /// Selects the row named `name`.
    #[track_caller]
    fn select(app: &mut App, name: &str) {
        let at = app
            .rows
            .iter()
            .position(|&i| app.snap.entries[i].name[..] == *name.as_bytes())
            .unwrap_or_else(|| panic!("{name} is not listed"));
        app.move_to(at);
    }

    /// The line of `frame` that shows `name`.
    #[track_caller]
    fn line<'a>(frame: &'a str, name: &str) -> &'a str {
        frame
            .lines()
            .find(|l| l.contains(name))
            .unwrap_or_else(|| panic!("{name} missing:\n{frame}"))
    }

    fn meta(kind: Kind, size: u64, ino: u64) -> Meta {
        Meta {
            kind,
            apparent: size,
            alloc: size,
            mtime: 0,
            dev: 1,
            ino,
            nlink: 1,
            mount: 0,
        }
    }

    fn add_dir(m: &mut Model, parent: Ix, name: &str, ino: u64) -> Ix {
        let d = m
            .tree
            .add_dir(parent, name.as_bytes(), &meta(Kind::Dir, 0, ino));
        let own = m.tree.own(d);
        m.tree.propagate_add(d, &own);
        d
    }

    fn add_file(m: &mut Model, parent: Ix, name: &str, size: u64, ino: u64) -> Ix {
        let f = m.tree.add_file(
            parent,
            name.as_bytes(),
            &meta(Kind::File, size, ino),
            false,
            None,
        );
        let own = m.tree.own(f);
        m.tree.propagate_add(parent, &own);
        f
    }

    fn complete(m: &mut Model, dir: Ix) {
        m.tree.dir_mut(dir).expect("directory").state = DirState::Complete;
    }

    fn summary_text(app: &App) -> Option<String> {
        summary(app).map(|s| s.content.into_owned())
    }

    fn tempdir() -> tempfile::TempDir {
        tempfile::tempdir_in(dunce::canonicalize(std::env::temp_dir()).expect("temp dir"))
            .expect("tempdir")
    }

    fn write(p: &Path, bytes: usize) {
        fs::create_dir_all(p.parent().expect("parent")).expect("mkdir");
        fs::write(p, vec![7u8; bytes]).expect("write");
    }

    fn engine(journal: Option<&Path>, read_only: bool) -> Engine<PortableBackend> {
        Engine::start(
            PortableBackend::default(),
            EngineOptions {
                workers: 2,
                journal_dir: journal.map(Path::to_path_buf),
                read_only,
                ..Default::default()
            },
        )
    }

    fn scan(e: &Engine<PortableBackend>, root: &Path) {
        e.send(Command::Scan {
            path: root.to_path_buf(),
            opts: JobOptions::default(),
        });
        assert!(e.wait_idle(Some(Duration::from_secs(20))));
    }

    /// Confirms a permanent delete of what the dialog targets and lets the engine carry it out.
    fn delete_permanently(app: &mut App, e: &Engine<PortableBackend>, t: Instant) {
        app.key(key(KeyCode::Char('D')), t);
        for c in "delete".chars() {
            app.key(key(KeyCode::Char(c)), t);
        }
        for fx in app.key(key(KeyCode::Enter), t) {
            let Effect::Send(cmd) = fx else {
                panic!("expected a command");
            };
            e.send(cmd);
        }
        assert!(e.wait_idle(Some(Duration::from_secs(20))));
    }

    #[test]
    fn space_marks_and_moves_down() {
        let t0 = Instant::now();
        let mut app = App::new(Theme::no_color(), t0);
        let es = vec![
            test_entry(0, "a", 3000),
            test_entry(1, "b", 2000),
            test_entry(2, "c", 1000),
        ];
        app.refresh(snap(es.clone(), 1), t0);
        assert_eq!(summary_text(&app), None);
        assert!(!stale(&app));
        assert!(app.key(key(KeyCode::Char(' ')), t0).is_empty());
        assert!(stale(&app), "a new mark needs a read");
        assert_eq!(app.selected_index(), Some(1));
        app.key(key(KeyCode::Char(' ')), t0);
        assert_eq!(app.selected_index(), Some(2));
        assert_eq!(
            summary_text(&app),
            Some(format!("● 2 marked ({})", human(5000)))
        );
        assert!(gutter(&app, &es[0]).content == "●");
        assert!(gutter(&app, &es[2]).content == " ");
        // On the last row the cursor stays; a second Space unmarks.
        app.key(key(KeyCode::Char(' ')), t0);
        app.key(key(KeyCode::Char(' ')), t0);
        assert_eq!(app.selected_index(), Some(2));
        app.key(key(KeyCode::Char('g')), t0);
        app.key(key(KeyCode::Char(' ')), t0);
        assert_eq!(
            summary_text(&app),
            Some(format!("● 1 marked ({})", human(2000)))
        );
        assert!(gutter(&app, &es[0]).content == " ");
        assert!(gutter(&app, &es[1]).content == "●");
        clear(&mut app);
        assert_eq!(summary_text(&app), None);
        assert!(gutter(&app, &es[1]).content == " ");
    }

    #[test]
    fn space_on_shared_row_is_refused() {
        let t0 = Instant::now();
        let mut app = App::new(Theme::no_color(), t0);
        let shared = EntryData {
            pseudo: true,
            ..test_entry(1, "(shared between entries)", 5000)
        };
        let gone = test_entry(2, "gone", 4000);
        let a = test_entry(0, "a", 10);
        app.refresh(snap(vec![shared.clone(), gone.clone(), a.clone()], 1), t0);
        // A deleted entry stays in its row until the next re-sort.
        let ghost = EntryData {
            pseudo: true,
            ghost: true,
            ..gone
        };
        app.refresh(snap(vec![shared.clone(), ghost, a], 2), t0);
        assert!(app.snap.entries[app.rows[1]].ghost);
        for row in 0..2 {
            app.move_to(row);
            app.key(key(KeyCode::Char(' ')), t0);
            assert!(app.marks.is_empty());
            assert_eq!(app.selected_index(), Some(row), "the cursor stays");
            assert_eq!(app.status.as_deref(), Some("only entries can be marked"));
        }
        assert!(!stale(&app));
        // The shared row has the directory's reference; marking the directory from its parent
        // must not put a mark on it.
        assert!(gutter(&app, &shared).content == " ");
    }

    #[test]
    fn marks_survive_navigation_and_rescan_swap() {
        let tmp = tempdir();
        let root = tmp.path();
        write(&root.join("sub/inner/f"), 4000);
        write(&root.join("sub/other.bin"), 1000);
        let e = engine(None, false);
        scan(&e, root);
        let t = Instant::now();
        let mut app = App::new(Theme::no_color(), t);
        e.read(|m| sync(&mut app, m, t));
        select(&mut app, "sub");
        app.key(key(KeyCode::Enter), t);
        e.read(|m| sync(&mut app, m, t));
        select(&mut app, "inner");
        app.key(key(KeyCode::Char(' ')), t);
        let old = app.marks.items()[0].target.node;
        app.key(key(KeyCode::Left), t);
        e.read(|m| sync(&mut app, m, t));
        assert!(!stale(&app));
        let out = render(&mut app, t, 90, 8);
        assert!(line(&out, "sub/").starts_with("  "), "{out}");
        assert!(line(&out, "1 marked").contains("● 1 marked"), "{out}");

        select(&mut app, "sub");
        app.key(key(KeyCode::Enter), t);
        e.read(|m| sync(&mut app, m, t));
        let out = render(&mut app, t, 90, 8);
        assert!(line(&out, "inner/").starts_with('●'), "{out}");
        assert!(line(&out, "other.bin").starts_with("  "), "{out}");

        // Rescanning sub replaces its children with new nodes.
        e.send(Command::Rescan {
            node: app.snap.dir_node.expect("sub"),
        });
        assert!(e.wait_idle(Some(Duration::from_secs(20))));
        e.read(|m| sync(&mut app, m, t));
        let inner = app
            .snap
            .entries
            .iter()
            .find(|en| en.name == b"inner")
            .expect("inner")
            .node;
        assert_ne!(Some(inner), old, "the rescan swapped the node");
        assert_eq!(app.marks.items().len(), 1);
        assert_eq!(app.marks.items()[0].target.node, Some(inner));
        let out = render(&mut app, t, 90, 8);
        assert!(line(&out, "inner/").starts_with('●'), "{out}");
        e.shutdown(Duration::from_millis(200));
    }

    #[test]
    fn nested_marks_count_once() {
        let mut m = Model::default();
        let root = m.tree.new_root(b"/r", &meta(Kind::Dir, 0, 1), true);
        let a = add_dir(&mut m, root, "a", 2);
        add_file(&mut m, a, "b", 6000, 3);
        add_file(&mut m, a, "c", 2000, 4);
        add_file(&mut m, root, "d", 1000, 5);
        for d in [a, root] {
            complete(&mut m, d);
        }
        let t = Instant::now();
        let mut app = App::new(Theme::no_color(), t);
        sync(&mut app, &m, t);
        select(&mut app, "a");
        app.key(key(KeyCode::Char(' ')), t);
        app.key(key(KeyCode::Up), t);
        app.key(key(KeyCode::Enter), t);
        sync(&mut app, &m, t);
        select(&mut app, "b");
        app.key(key(KeyCode::Char(' ')), t);
        app.key(key(KeyCode::Left), t);
        sync(&mut app, &m, t);
        let a_total = m.tree.total(a).alloc;
        assert_eq!(app.marks.total(), (a_total, false));
        assert_eq!(
            summary_text(&app),
            Some(format!("● 2 marked ({})", human(a_total)))
        );
        // b marked first, inside a marked after it: still counted once.
        let mut app2 = App::new(Theme::no_color(), t);
        sync(&mut app2, &m, t);
        select(&mut app2, "a");
        app2.key(key(KeyCode::Enter), t);
        sync(&mut app2, &m, t);
        select(&mut app2, "b");
        app2.key(key(KeyCode::Char(' ')), t);
        app2.key(key(KeyCode::Left), t);
        sync(&mut app2, &m, t);
        select(&mut app2, "a");
        app2.key(key(KeyCode::Char(' ')), t);
        sync(&mut app2, &m, t);
        assert_eq!(app2.marks.total(), (a_total, false));

        app.key(key(KeyCode::Char('d')), t);
        let out = render(&mut app, t, 100, 20);
        assert!(out.contains("Move a/ to the Trash?"), "{out}");
        assert!(
            line(&out, "a/b").contains("inside another marked item"),
            "{out}"
        );
        let fx = app.key(key(KeyCode::Char('y')), t);
        let a_ref = m.tree.node_ref(a);
        assert!(
            matches!(fx.as_slice(), [Effect::Send(Command::Delete { items, .. })] if items == &[a_ref]),
            "{fx:?}"
        );
    }

    #[test]
    fn totals_follow_live_sizes() {
        let mut m = Model::default();
        let root = m.tree.new_root(b"/r", &meta(Kind::Dir, 0, 1), true);
        let big = add_dir(&mut m, root, "big", 2);
        add_file(&mut m, big, "f1", 100_000, 3);
        let t = Instant::now();
        let mut app = App::new(Theme::no_color(), t);
        sync(&mut app, &m, t);
        select(&mut app, "big");
        app.key(key(KeyCode::Char(' ')), t);
        assert!(stale(&app));
        sync(&mut app, &m, t);
        assert!(!stale(&app), "a read takes the new mark in");
        let size = |m: &Model| human(m.tree.total(big).alloc);
        let first = summary_text(&app);
        assert_eq!(first, Some(format!("● 1 marked (at least {})", size(&m))));
        add_file(&mut m, big, "f2", 2_000_000, 4);
        sync(&mut app, &m, t);
        assert_eq!(
            summary_text(&app),
            Some(format!("● 1 marked (at least {})", size(&m)))
        );
        assert_ne!(summary_text(&app), first, "the total grew with the scan");
        complete(&mut m, big);
        sync(&mut app, &m, t);
        assert_eq!(
            summary_text(&app),
            Some(format!("● 1 marked ({})", size(&m)))
        );
        // The size mode of the read decides the size.
        app.size_mode = crate::SizeMode::Apparent;
        sync(&mut app, &m, t);
        assert_eq!(app.marks.total().0, m.tree.total(big).apparent);
    }

    #[test]
    fn deleted_marks_are_dropped() {
        let tmp = tempdir();
        let root = tmp.path();
        write(&root.join("junk/b.bin"), 50_000);
        write(&root.join("file.bin"), 3_000);
        write(&root.join("keep.bin"), 1_000);
        let journal = tempdir();
        let e = engine(Some(journal.path()), false);
        scan(&e, root);
        let t = Instant::now();
        let mut app = App::new(Theme::no_color(), t);
        e.read(|m| sync(&mut app, m, t));
        for name in ["junk", "file.bin"] {
            select(&mut app, name);
            app.key(key(KeyCode::Char(' ')), t);
        }
        e.read(|m| sync(&mut app, m, t));
        assert_eq!(app.marks.items().len(), 2);
        // file.bin changes after the scan: the delete leaves it alone.
        write(&root.join("file.bin"), 4_000);
        delete_permanently(&mut app, &e, t);
        e.read(|m| sync(&mut app, m, t));
        e.shutdown(Duration::from_millis(200));
        assert!(!root.join("junk").exists());
        let status = app.status.clone().unwrap_or_default();
        assert!(status.contains("deleted 1"), "{status}");
        assert!(status.contains("1 changed since the scan"), "{status}");
        let left: Vec<_> = app
            .marks
            .items()
            .iter()
            .map(|it| it.target.path.clone())
            .collect();
        assert_eq!(left, [root.join("file.bin")]);
        let out = render(&mut app, t, 90, 8);
        assert!(line(&out, "file.bin").starts_with('●'), "{out}");
        assert!(line(&out, "keep.bin").starts_with("  "), "{out}");
    }

    #[test]
    fn refused_delete_keeps_the_marks() {
        let tmp = tempdir();
        let root = tmp.path();
        write(&root.join("junk/b.bin"), 50_000);
        let journal = tempdir();
        let e = engine(Some(journal.path()), true);
        scan(&e, root);
        let t = Instant::now();
        let mut app = App::new(Theme::no_color(), t);
        e.read(|m| sync(&mut app, m, t));
        select(&mut app, "junk");
        app.key(key(KeyCode::Char(' ')), t);
        delete_permanently(&mut app, &e, t);
        e.read(|m| sync(&mut app, m, t));
        e.shutdown(Duration::from_millis(200));
        assert!(root.join("junk").exists());
        assert_eq!(app.marks.items().len(), 1);
        let out = render(&mut app, t, 90, 8);
        assert!(line(&out, "junk/").starts_with('●'), "{out}");
        assert_eq!(app.snap.notice.as_deref(), Some("delete: read-only mode"));
    }

    /// A mark of `path` with no reference, as after a rescan swap.
    fn lost(path: &str, ident: Option<Ident>) -> Item {
        Item {
            target: Target {
                node: None,
                path: path.into(),
                ident,
            },
            is_dir: false,
            size: 1,
            items: 1,
            shared_out: 0,
            scanning: false,
            inside: false,
        }
    }

    #[test]
    fn many_marks_index_quickly() {
        let mut marks = Marks::default();
        for i in 0..10_000 {
            let path = format!("/home/jo/d{}/e{}/f{i}", i % 100, i % 7);
            marks.items.push(lost(&path, None));
        }
        marks.items.push(lost("/home/jo/d1", None));
        let t = Instant::now();
        marks.index();
        let took = t.elapsed();
        println!("10,001 marks indexed in {took:?}");
        assert!(took < Duration::from_secs(2), "{took:?}");
        assert_eq!(marks.items.iter().filter(|it| it.inside).count(), 100);
        assert_eq!(marks.total(), (10_000 - 100 + 1, false));
    }

    #[test]
    fn lost_references_are_found_by_path() {
        let mut m = Model::default();
        let root = m.tree.new_root(b"/r", &meta(Kind::Dir, 0, 1), true);
        let big = add_dir(&mut m, root, "big", 2);
        let files: Vec<Ix> = (0..1000)
            .map(|i| add_file(&mut m, big, &format!("f{i}"), 10, 100 + i))
            .collect();
        let other = add_dir(&mut m, root, "other", 3);
        let g = add_file(&mut m, other, "g", 10, 4);
        let file = |ino| {
            Some(Ident {
                dev_ino: (1, ino),
                kind: Kind::File,
            })
        };
        let t = Instant::now();
        let mut app = App::new(Theme::no_color(), t);
        app.marks.items = vec![
            lost("/r/big/f999", file(1099)),
            lost("/r/other/g", file(4)),
            lost("/r/big/f10", None),
            lost("/r/big/missing", None),
            lost("/r/big/f20", file(7)),
            lost("/r/gone/f1", None),
            lost("/r/big/f500", file(600)),
            lost("/r", None),
        ];
        let found: Vec<_> = read(&m, &app)
            .found
            .iter()
            .map(|f| f.as_ref().map(|f| f.node))
            .collect();
        let at = |ix| Some(m.tree.node_ref(ix));
        assert_eq!(
            found,
            [
                at(files[999]),
                at(g),
                at(files[10]),
                None,
                None,
                None,
                at(files[500]),
                at(root),
            ]
        );
    }

    /// A scan 14 s in with two marked entries, one of them still being scanned.
    fn marked_view(theme: Theme) -> ratatui::buffer::Buffer {
        let t0 = Instant::now();
        let mut app = App::new(theme, t0);
        let gib = 1 << 30;
        let dir = |i, name: &str, size, mark| EntryData {
            is_dir: true,
            mark,
            items: 1000,
            ..test_entry(i, name, size)
        };
        let entries = vec![
            dir(0, "Library", 112 * gib, Mark::Scanning),
            dir(1, "dev", 78 * gib, Mark::None),
            dir(2, ".android", 51 * gib, Mark::None),
            test_entry(3, "notes.sqlite", 625 << 20),
        ];
        let total: u64 = entries.iter().map(|e| e.size).sum();
        let totals = Totals {
            apparent: total,
            alloc: total,
            items: 3004,
            ..Totals::default()
        };
        app.refresh(
            Snapshot {
                dir_path: "/home/jo".into(),
                dir_node: Some(test_entry(99, "/home/jo", 0).node),
                at_root: true,
                dir_total: totals,
                dir_mark: Some(Mark::Scanning),
                root_total: totals,
                entries,
                busy: true,
                version: 1,
                ..Default::default()
            },
            t0,
        );
        app.key(key(KeyCode::Char(' ')), t0);
        app.key(key(KeyCode::Down), t0);
        app.key(key(KeyCode::Char(' ')), t0);
        render_buffer(&mut app, t0 + Duration::from_secs(14), 100, 10)
    }

    #[test]
    fn marked_rows_show_a_glyph_in_mono() {
        let out = text(&marked_view(Theme::no_color()));
        insta::assert_snapshot!("marks_mono", out);
        assert!(line(&out, "Library/").starts_with('●'), "{out}");
        assert!(line(&out, ".android/").starts_with('●'), "{out}");
        assert!(line(&out, "dev/").starts_with("  "), "{out}");
        assert!(out.contains("● 2 marked (at least 163.0 GiB)"), "{out}");

        let buf = marked_view(Theme::ansi16());
        insta::assert_snapshot!("marks_ansi16", styled(&buf));
        let mark = Theme::ansi16().mark;
        assert_eq!(style_at(&buf, "● 2 marked").fg, mark.fg);
        assert_ne!(mark.fg, None);
        assert_ne!(mark.fg, Some(Color::Reset));
    }
}
