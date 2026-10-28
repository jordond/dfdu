use std::time::Instant;

use dfdu_engine::Model;
use dfdu_tree::{DirState, Ix, Kind, Meta};
use ratatui::buffer::Buffer;
use ratatui::crossterm::event::{KeyCode, MouseButton, MouseEvent, MouseEventKind};

use super::*;
use crate::app::tests::snap;
use crate::extras;
use crate::snapshot::{self, EntryData, test_entry};
use crate::tests::{key, render, render_buffer, styled};
use crate::theme::Theme;

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

struct Fixture {
    m: Model,
    ino: u64,
}

impl Fixture {
    fn new() -> (Self, Ix) {
        let mut m = Model::default();
        let root = m.tree.new_root(b"/r", &meta(Kind::Dir, 0, 1), true);
        (Fixture { m, ino: 1 }, root)
    }

    fn dir(&mut self, parent: Ix, name: &str) -> Ix {
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

    fn file(&mut self, parent: Ix, name: &str, size: u64) {
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
    }
}

/// `/r` with `big/` (`a/` with two files, and `b`), `mid/` (three files), `small` and dotfile
/// `.hid`.
fn tree() -> Fixture {
    let (mut fx, root) = Fixture::new();
    fx.m.tree.dir_mut(root).expect("root").state = DirState::Complete;
    let big = fx.dir(root, "big");
    let a = fx.dir(big, "a");
    fx.file(a, "a1", 30_000);
    fx.file(a, "a2", 10_000);
    fx.file(big, "b", 20_000);
    let mid = fx.dir(root, "mid");
    for (i, size) in [12_000, 8_000, 5_000].into_iter().enumerate() {
        fx.file(mid, &format!("m{i}"), size);
    }
    fx.file(root, "small", 9_000);
    fx.file(root, ".hid", 4_000);
    fx
}

/// Reads the model as the run loop does for a frame with a new snapshot: the snapshot, then
/// the extras until none is stale.
fn sync(app: &mut App, m: &Model, now: Instant) {
    let mut target = app.target.clone();
    let snap = snapshot::build(m, &mut target, app.size_mode);
    app.target = target;
    app.refresh(snap, now);
    for _ in 0..3 {
        let budget = dfdu_scan::Budget::new(1 << 20);
        let x = extras::read(m, app, &budget, Default::default());
        extras::apply(app, x);
        if !app.extras_stale() {
            return;
        }
    }
    panic!("the extras stay stale");
}

fn treemap_app(m: &Model, now: Instant) -> App {
    let mut app = App::new(Theme::no_color(), now);
    app.key(key(KeyCode::Char('2')), now);
    sync(&mut app, m, now);
    app
}

fn selected(app: &App) -> String {
    app.selected_entry()
        .map(|e| String::from_utf8_lossy(&e.name).into_owned())
        .unwrap_or_default()
}

#[test]
fn read_finds_the_levels_below_the_largest_directories() {
    let fx = tree();
    let t0 = Instant::now();
    let mut app = treemap_app(&fx.m, t0);
    let below = &app.extras.treemap.below;
    assert_eq!(below.len(), 2, "big and mid");
    let big = app
        .snap
        .entries
        .iter()
        .find(|e| e.name == b"big")
        .expect("big");
    let kids = &below[&big.node];
    let names: Vec<&[u8]> = kids.items.iter().map(|k| &k.name[..]).collect();
    assert_eq!(names, [&b"a"[..], b"b"]);
    // Depth 2 reads one level below the top only.
    assert!(kids.items[0].kids.is_none());

    app.key(key(KeyCode::Char('+')), t0);
    assert!(app.extras_stale());
    sync(&mut app, &fx.m, t0);
    let big = app
        .snap
        .entries
        .iter()
        .find(|e| e.name == b"big")
        .expect("big");
    let a = &app.extras.treemap.below[&big.node].items[0];
    let a_kids = a.kids.as_ref().expect("depth 3 reads a/");
    assert_eq!(a_kids.items.len(), 2);
    assert_eq!(a_kids.items[0].size, 30_000);
}

#[test]
fn read_skips_hidden_entries_when_they_are_hidden_and_keeps_the_rest_total() {
    let (mut fx, root) = Fixture::new();
    fx.m.tree.dir_mut(root).expect("root").state = DirState::Complete;
    let d = fx.dir(root, "d");
    for i in 0..(MAX_TILES as u64 + 10) {
        fx.file(d, &format!("f{i:03}"), 1000 + i);
    }
    fx.file(d, ".dot", 50_000);
    let t0 = Instant::now();
    let mut app = treemap_app(&fx.m, t0);
    let node = app.snap.entries[0].node;
    let kids = &app.extras.treemap.below[&node];
    assert_eq!(kids.items.len(), MAX_TILES);
    assert_eq!(kids.items[0].name, b".dot");
    let shown: u64 = kids.items.iter().map(|k| k.size).sum();
    assert_eq!(shown + kids.rest.1, fx.m.tree.total(Ix(d.0)).alloc);
    assert_eq!(kids.rest.0, 11);

    app.key(key(KeyCode::Char('e')), t0);
    sync(&mut app, &fx.m, t0);
    let kids = &app.extras.treemap.below[&node];
    assert!(kids.items.iter().all(|k| k.name[0] != b'.'));
}

#[test]
fn read_stops_at_the_budget() {
    let (mut fx, root) = Fixture::new();
    let d = fx.dir(root, "d");
    fx.file(d, "x", 10);
    let mut budget = 0;
    let key = Key {
        dir: None,
        sort_gen: 0,
        depth: 2,
        mode: SizeMode::Disk,
        hide_hidden: false,
    };
    assert!(read_kids(&fx.m.tree, d, None, 1, &key, &mut budget).is_none());
    budget = 1;
    assert!(read_kids(&fx.m.tree, d, None, 1, &key, &mut budget).is_some());
    assert_eq!(budget, 0);
}

#[test]
fn nothing_is_read_while_the_list_is_shown() {
    let fx = tree();
    let t0 = Instant::now();
    let mut app = App::new(Theme::no_color(), t0);
    sync(&mut app, &fx.m, t0);
    assert!(app.extras.treemap.below.is_empty());
    assert!(!app.extras_stale());
}

#[test]
fn draws_nested_tiles() {
    let fx = tree();
    let t0 = Instant::now();
    let mut app = treemap_app(&fx.m, t0);
    insta::assert_snapshot!("treemap_100x30", render(&mut app, t0, 100, 30));
}

#[test]
fn draws_three_levels_in_colour() {
    let fx = tree();
    let t0 = Instant::now();
    let mut app = treemap_app(&fx.m, t0);
    app.theme = Theme::ansi16();
    app.key(key(KeyCode::Char('+')), t0);
    sync(&mut app, &fx.m, t0);
    insta::assert_snapshot!(
        "treemap_depth3_ansi16",
        styled(&render_buffer(&mut app, t0, 90, 24))
    );
}

#[test]
fn age_colour_has_a_legend_and_text() {
    let fx = tree();
    let t0 = Instant::now();
    let mut app = treemap_app(&fx.m, t0);
    app.key(key(KeyCode::Char('p')), t0);
    let frame = render(&mut app, t0, 100, 30);
    assert!(frame.contains("colour: age"), "{frame}");
    assert!(frame.contains("■ week"), "{frame}");
    // Without colour, each tile still says how old it is.
    assert!(frame.contains("≤ older"), "{frame}");
}

#[test]
fn small_terminals_get_the_list() {
    let fx = tree();
    let t0 = Instant::now();
    let mut app = treemap_app(&fx.m, t0);
    let frame = render(&mut app, t0, 59, 20);
    assert!(!app.treemap.shown);
    assert!(!frame.contains("colour:"), "{frame}");
    // Arrows move the list rows then.
    app.key(key(KeyCode::Down), t0);
    assert_eq!(selected(&app), "mid");
    render(&mut app, t0, 60, 17);
    assert!(app.treemap.shown);
}

/// Every tile with a frame keeps its frame: no label runs over it, at any size.
#[test]
fn labels_stay_inside_their_tiles() {
    let (mut fx, root) = Fixture::new();
    fx.m.tree.dir_mut(root).expect("root").state = DirState::Complete;
    for i in 0..30u64 {
        let d = fx.dir(root, &format!("a-directory-with-a-rather-long-name-{i}"));
        for j in 0..6u64 {
            fx.file(
                d,
                &format!("file-with-a-long-name-{j}.bin"),
                (40 - i) * 1000 + j * 7,
            );
        }
    }
    let t0 = Instant::now();
    let mut app = treemap_app(&fx.m, t0);
    for (w, h) in [(60, 15), (80, 24), (100, 30), (133, 41), (200, 60)] {
        let buf = render_buffer(&mut app, t0, w, h);
        let l = app.treemap.layout.as_ref().expect("layout");
        assert!(!l.tiles.is_empty());
        for tile in &l.tiles {
            let r = tile.rect;
            if r.width < 4 || r.height < 3 {
                continue;
            }
            let at = |x: u16, y: u16| buf[(x, y)].symbol().to_string();
            for x in r.left() + 1..r.right() - 1 {
                for y in [r.top(), r.bottom() - 1] {
                    assert!(
                        matches!(at(x, y).as_str(), "─" | "━"),
                        "{w}x{h}: {:?} border at ({x},{y}) is {:?}",
                        tile.name,
                        at(x, y)
                    );
                }
            }
            for y in r.top() + 1..r.bottom() - 1 {
                for x in [r.left(), r.right() - 1] {
                    assert!(
                        matches!(at(x, y).as_str(), "│" | "┃"),
                        "{w}x{h}: {:?}",
                        tile.name
                    );
                }
            }
        }
    }
}

#[test]
fn many_small_entries_merge_into_one_tile() {
    let (mut fx, root) = Fixture::new();
    fx.m.tree.dir_mut(root).expect("root").state = DirState::Complete;
    fx.file(root, "huge", 1_000_000);
    for i in 0..300 {
        fx.file(root, &format!("tiny{i}"), 1000);
    }
    let t0 = Instant::now();
    let mut app = treemap_app(&fx.m, t0);
    let frame = render(&mut app, t0, 100, 30);
    assert!(frame.contains("+300 small"), "{frame}");
}

#[test]
fn arrows_move_between_tiles_and_enter_zooms() {
    let fx = tree();
    let t0 = Instant::now();
    let mut app = treemap_app(&fx.m, t0);
    render(&mut app, t0, 100, 30);
    assert_eq!(selected(&app), "big");
    let l = app.treemap.layout.as_ref().expect("layout");
    let rect = |name: &str| {
        l.tops
            .iter()
            .map(|&t| &l.tiles[t])
            .find(|t| t.name == name)
            .map(|t| t.rect)
            .expect(name)
    };
    let (big, mid) = (rect("big"), rect("mid"));
    // `big` is the largest tile: on the left, the others to its right.
    assert!(mid.x >= big.right(), "{big:?} {mid:?}");
    app.key(key(KeyCode::Char('l')), t0);
    let right_of_big = selected(&app);
    assert_ne!(right_of_big, "big");
    app.key(key(KeyCode::Left), t0);
    assert_eq!(selected(&app), "big");
    // Nothing further left: the selection stays.
    app.key(key(KeyCode::Char('h')), t0);
    assert_eq!(selected(&app), "big");

    app.key(key(KeyCode::Enter), t0);
    sync(&mut app, &fx.m, t0);
    assert!(app.snap.dir_path.ends_with("big"));
    render(&mut app, t0, 100, 30);
    app.key(key(KeyCode::Char('<')), t0);
    sync(&mut app, &fx.m, t0);
    assert_eq!(app.snap.dir_path, std::path::Path::new("/r"));
}

#[test]
fn click_selects_a_tile_and_a_second_click_zooms_in() {
    let fx = tree();
    let t0 = Instant::now();
    let mut app = treemap_app(&fx.m, t0);
    render(&mut app, t0, 100, 30);
    let l = app.treemap.layout.as_ref().expect("layout");
    let mid = l
        .tops
        .iter()
        .map(|&t| &l.tiles[t])
        .find(|t| t.name == "mid")
        .expect("mid")
        .rect;
    let click = MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column: mid.x + 2,
        row: mid.y + 2,
        modifiers: ratatui::crossterm::event::KeyModifiers::NONE,
    };
    crate::mouse::event(&mut app, click, t0).expect("handled");
    assert_eq!(selected(&app), "mid");
    let fx2 = crate::mouse::event(&mut app, click, t0).expect("handled");
    assert!(fx2.is_empty());
    sync(&mut app, &fx.m, t0);
    assert!(app.snap.dir_path.ends_with("mid"));
}

#[test]
fn tiles_hold_still_until_the_next_sort() {
    let t0 = Instant::now();
    let mut app = App::new(Theme::no_color(), t0);
    app.key(key(KeyCode::Char('2')), t0);
    let mut es: Vec<EntryData> = (0..6)
        .map(|i| test_entry(i, &format!("e{i}"), 6000 - i as u64 * 900))
        .collect();
    app.refresh(snap(es.clone(), 1), t0);
    render(&mut app, t0, 100, 30);
    let before: Vec<Rect> = layout_rects(&app);
    // e5 grows past everything, inside the idle window: no re-sort, no new layout.
    es[5].size = 50_000;
    app.refresh(snap(es.clone(), 2), t0);
    let frame = render(&mut app, t0, 100, 30);
    assert_eq!(layout_rects(&app), before);
    // The label shows the live size.
    assert!(frame.contains("48.8 KiB"), "{frame}");
    // A re-sort lays out again.
    app.key(key(KeyCode::Char('S')), t0);
    app.reorder(t0);
    render(&mut app, t0, 100, 30);
    assert_ne!(layout_rects(&app), before);
}

fn layout_rects(app: &App) -> Vec<Rect> {
    let l = app.treemap.layout.as_ref().expect("layout");
    l.tiles.iter().map(|t| t.rect).collect()
}

#[test]
fn marked_tiles_say_so() {
    let fx = tree();
    let t0 = Instant::now();
    let mut app = treemap_app(&fx.m, t0);
    app.key(key(KeyCode::Char(' ')), t0);
    let frame = render(&mut app, t0, 100, 30);
    assert!(frame.contains("✓ big/"), "{frame}");
}

#[test]
fn the_selected_tile_has_a_heavy_frame() {
    let fx = tree();
    let t0 = Instant::now();
    let mut app = treemap_app(&fx.m, t0);
    let buf: Buffer = render_buffer(&mut app, t0, 100, 30);
    let l = app.treemap.layout.as_ref().expect("layout");
    let big = l.tiles[l.tops[0]].rect;
    assert_eq!(buf[(big.x, big.y)].symbol(), "┏");
}

/// The acceptance bound: a directory of 10k children laid out at 200x60 in under 2 ms (release
/// build; a debug build gets more room).
#[test]
fn ten_thousand_rows_lay_out_quickly() {
    let t0 = Instant::now();
    let mut app = App::new(Theme::no_color(), t0);
    app.key(key(KeyCode::Char('2')), t0);
    let es: Vec<EntryData> = (0..10_000u32)
        .map(|i| test_entry(i, &format!("entry-{i}"), 1 + (i as u64 * 7919) % 100_000))
        .collect();
    app.refresh(snap(es, 1), t0);
    let area = Rect::new(0, 3, 200, 54);
    let mut best = Duration::MAX;
    for _ in 0..5 {
        let t = Instant::now();
        let l = build(&app, area);
        best = best.min(t.elapsed());
        assert!(l.tiles.iter().any(|t| t.small > 0));
    }
    let bound = if cfg!(debug_assertions) { 40 } else { 2 };
    assert!(best.as_millis() < bound, "{best:?}");
}

/// [`tree`] with the built-in rules, `big` matched as a Rust `target/`.
fn classified() -> Fixture {
    let mut fx = tree();
    let env = dfdu_rules::Env::with(dfdu_rules::Platform::Linux, &[("HOME", "/home/u")]);
    let rules = dfdu_rules::builtin(&env).0;
    let rule = rules
        .rules()
        .iter()
        .position(|r| r.id == "rust.target")
        .unwrap() as u16;
    fx.m.rules = Some(Arc::new(rules));
    let root = fx.m.tree.roots().next().unwrap();
    let big =
        fx.m.tree
            .children(root)
            .find(|&c| fx.m.tree.name(c) == b"big")
            .unwrap();
    let class = dfdu_tree::Class {
        rule,
        safety: dfdu_rules::Safety::Regenerable as u8,
        degraded: None,
        flags: 0,
    };
    fx.m.tree.set_class(big, Some(class));
    let mid =
        fx.m.tree
            .children(root)
            .find(|&c| fx.m.tree.name(c) == b"mid")
            .unwrap();
    let m0 =
        fx.m.tree
            .children(mid)
            .find(|&c| fx.m.tree.name(c) == b"m0")
            .unwrap();
    let rules = fx.m.rules.as_ref().unwrap();
    let cache = rules
        .rules()
        .iter()
        .position(|r| r.id == "generic.cachedir-tag")
        .unwrap();
    let class = dfdu_tree::Class {
        rule: cache as u16,
        safety: dfdu_rules::Safety::Safe as u8,
        ..class
    };
    fx.m.tree.set_class(m0, Some(class));
    fx
}

#[test]
fn category_colour_hatches_cleanup_candidates() {
    let fx = classified();
    let t0 = Instant::now();
    let mut app = treemap_app(&fx.m, t0);
    for _ in 0..2 {
        app.key(key(KeyCode::Char('p')), t0);
    }
    let frame = render(&mut app, t0, 100, 30);
    assert!(frame.contains("colour: category"), "{frame}");
    assert!(frame.contains("╱ cleanup candidate"), "{frame}");
    insta::assert_snapshot!("treemap_category_100x30", frame);
}

#[test]
fn list_and_details_show_the_category() {
    let fx = classified();
    let t0 = Instant::now();
    let mut app = App::new(Theme::no_color(), t0);
    crate::details::toggle(&mut app);
    sync(&mut app, &fx.m, t0);
    let frame = render(&mut app, t0, 120, 24);
    assert!(frame.contains("build"), "{frame}");
    assert!(
        !frame.contains("by category"),
        "a match is one category: {frame}"
    );
    assert!(frame.contains("Rust build output (target/)"), "{frame}");
    insta::assert_snapshot!("list_category_120x24", frame);
    // `mid` has a match inside: its details break it down by category.
    app.key(key(KeyCode::Down), t0);
    sync(&mut app, &fx.m, t0);
    let frame = render(&mut app, t0, 120, 24);
    assert!(frame.contains("by category"), "{frame}");
    assert!(frame.contains("OS and user caches"), "{frame}");
}
