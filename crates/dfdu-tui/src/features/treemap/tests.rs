use std::time::Instant;

use dfdu_engine::Model;
use dfdu_tree::{DirState, HostPath, Ix};
use ratatui::buffer::Buffer;
use ratatui::crossterm::event::{KeyCode, MouseButton, MouseEvent, MouseEventKind};

use super::place::{Tile, build};
use super::*;
use crate::model::snapshot::{EntryData, test_entry};
use crate::tests::{
    Fixture, classified, press, render, render_buffer, snap_without_totals, styled, sync, tree,
};
use crate::theme::Theme;

pub(crate) fn treemap_app(m: &Model, now: Instant) -> App {
    let mut app = App::new(Theme::no_color(), now);
    app.key(press(KeyCode::Char('2')), now);
    sync(&mut app, m, now);
    app
}

/// [`treemap_app`] one level deeper (`+`): every tile shows what is in it.
pub(crate) fn nested_app(m: &Model, now: Instant) -> App {
    let mut app = treemap_app(m, now);
    app.key(press(KeyCode::Char('+')), now);
    sync(&mut app, m, now);
    app
}

/// The Map of a directory of `entries`, without totals.
pub(crate) fn map_of(entries: Vec<EntryData>, now: Instant) -> App {
    let mut app = App::new(Theme::no_color(), now);
    app.key(press(KeyCode::Char('2')), now);
    app.refresh(snap_without_totals(entries, 1), now);
    app
}

/// The symbols of row `y` of `buf`, across its area.
pub(crate) fn buffer_row(buf: &Buffer, y: u16) -> String {
    (buf.area.left()..buf.area.right())
        .map(|x| buf[(x, y)].symbol())
        .collect()
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
    // Depths 1 and 2 read one level below the top only.
    assert!(kids.items[0].kids.is_none());

    for _ in 0..2 {
        app.key(press(KeyCode::Char('+')), t0);
    }
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

    app.key(press(KeyCode::Char('e')), t0);
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
        types: 0,
    };
    let all = TypeFilter::default();
    assert!(read_kids(&fx.m, d, None, 1, &key, &all, &mut budget).is_none());
    budget = 1;
    assert!(read_kids(&fx.m, d, None, 1, &key, &all, &mut budget).is_some());
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
fn draws_three_levels_in_color() {
    let fx = tree();
    let t0 = Instant::now();
    let mut app = treemap_app(&fx.m, t0);
    app.theme = Theme::ansi16();
    for _ in 0..2 {
        app.key(press(KeyCode::Char('+')), t0);
    }
    sync(&mut app, &fx.m, t0);
    insta::assert_snapshot!(
        "treemap_depth3_ansi16",
        styled(&render_buffer(&mut app, t0, 90, 24))
    );
}

#[test]
fn age_color_has_a_legend_and_text() {
    let fx = tree();
    let t0 = Instant::now();
    let mut app = treemap_app(&fx.m, t0);
    app.key(press(KeyCode::Char('p')), t0);
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
    app.key(press(KeyCode::Down), t0);
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

/// As the app sees a scan: the map is drawn before the matches are decided, and the matches
/// lie deep inside the top-level directories (`dev/shop/node_modules`). Once decided, `dev/`
/// takes the colour of what is matched inside it, and the legend names it.
#[test]
fn category_colour_follows_matches_found_later_and_deep_inside() {
    let (mut fx, root) = Fixture::new();
    let dev = fx.dir(root, "dev");
    let shop = fx.dir(dev, "shop");
    let deps = fx.dir(shop, "node_modules");
    fx.file(deps, "react.js", 50_000);
    fx.file(shop, "app.js", 1_000);
    let docs = fx.dir(root, "docs");
    fx.file(docs, "taxes.pdf", 20_000);
    fx.m.tree.dir_mut(root).expect("root").state = DirState::Complete;
    let env = dfdu_rules::Env::with(dfdu_rules::Platform::Linux, &[("HOME", "/home/u")]);
    let rules = dfdu_rules::builtin(&env).0;
    let rule = rules.index("node.node_modules").expect("the rule");
    fx.m.rules = Some(Arc::new(rules));
    let t0 = Instant::now();
    let mut app = treemap_app(&fx.m, t0);
    app.theme = Theme::named("dark", crate::theme::ColorLevel::TrueColor, None).expect("dark");
    let slot_of = |app: &mut App, name: &str| {
        render(app, t0, 100, 30);
        let l = app.treemap.layout.as_ref().expect("layout");
        let i = (l.tiles.iter().position(|t| t.name == name)).expect(name);
        super::tile::base_style(app, &l.tiles, i, 0).1
    };
    assert_eq!(slot_of(&mut app, "dev"), None, "nothing matched yet");
    assert!(!render(&mut app, t0, 100, 30).contains("■ build"));
    // The scan decides the match.
    let class = dfdu_tree::Class {
        rule,
        safety: dfdu_tree::Safety::Regenerable,
        degraded: None,
        flags: 0,
    };
    fx.m.tree.set_class(deps, Some(class));
    sync(&mut app, &fx.m, t0);
    let build = crate::theme::Category::Build as usize;
    assert_eq!(slot_of(&mut app, "dev"), Some(build));
    assert_eq!(slot_of(&mut app, "docs"), None, "nothing matched in docs/");
    let frame = render(&mut app, t0, 100, 30);
    assert!(frame.contains("colour: category  ■ build"), "{frame}");
}

#[test]
fn an_empty_directory_has_an_empty_state() {
    let t0 = Instant::now();
    let mut app = App::new(Theme::no_color(), t0);
    app.refresh(snap_without_totals(Vec::new(), 1), t0);
    app.key(press(KeyCode::Char('2')), t0);
    let frame = render(&mut app, t0, 100, 30);
    assert!(frame.contains("╭ Map ─"), "{frame}");
    assert!(frame.contains("This directory is empty."), "{frame}");
    assert!(
        frame.contains("Backspace  parent directory      Esc  back to Files"),
        "{frame}"
    );
    insta::assert_snapshot!("map_empty_100x30", frame);
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
    assert!(frame.contains("300 more · 293.0 KiB"), "{frame}");
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
    app.key(press(KeyCode::Char('l')), t0);
    let right_of_big = selected(&app);
    assert_ne!(right_of_big, "big");
    app.key(press(KeyCode::Left), t0);
    assert_eq!(selected(&app), "big");
    // Nothing further left: the selection stays.
    app.key(press(KeyCode::Char('h')), t0);
    assert_eq!(selected(&app), "big");

    app.key(press(KeyCode::Enter), t0);
    sync(&mut app, &fx.m, t0);
    assert!(app.snap.dir_path.to_local().ends_with("big"));
    render(&mut app, t0, 100, 30);
    app.key(press(KeyCode::Char('<')), t0);
    sync(&mut app, &fx.m, t0);
    assert_eq!(app.snap.dir_path, HostPath::from("/r"));
}

#[test]
fn the_line_under_the_map_shows_the_selected_tiles_place() {
    let fx = tree();
    let t0 = Instant::now();
    let mut app = treemap_app(&fx.m, t0);
    // The line under the map is the third from the bottom, above the status and the hints.
    let info = |app: &mut App| {
        let frame = render(app, t0, 100, 30);
        frame.lines().rev().nth(2).unwrap_or_default().to_owned()
    };
    let line = info(&mut app);
    let tops = app.treemap.layout.as_ref().map_or(0, |l| l.tops.len());
    assert!(tops > 1, "{line}");
    assert!(line.ends_with(&format!(" 1/{tops}")), "{line}");
    app.key(press(KeyCode::Char('l')), t0);
    let line = info(&mut app);
    assert!(!line.ends_with(&format!(" 1/{tops}")), "{line}");
    assert!(line.contains(&format!("/{tops}")), "{line}");
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
    crate::input::mouse::event(&mut app, click, t0).expect("handled");
    assert_eq!(selected(&app), "mid");
    let fx2 = crate::input::mouse::event(&mut app, click, t0).expect("handled");
    assert!(fx2.is_empty());
    sync(&mut app, &fx.m, t0);
    assert!(app.snap.dir_path.to_local().ends_with("mid"));
}

#[test]
fn tiles_hold_still_until_the_next_sort() {
    let t0 = Instant::now();
    let mut es: Vec<EntryData> = (0..6)
        .map(|i| test_entry(i, &format!("e{i}"), 6000 - u64::from(i) * 900))
        .collect();
    let mut app = map_of(es.clone(), t0);
    render(&mut app, t0, 100, 30);
    let before: Vec<Rect> = layout_rects(&app);
    // e5 grows past everything, inside the idle window: no re-sort, no new layout.
    es[5].size = 50_000;
    app.refresh(snap_without_totals(es.clone(), 2), t0);
    let frame = render(&mut app, t0, 100, 30);
    assert_eq!(layout_rects(&app), before);
    // The label shows the live size.
    assert!(frame.contains("48.8 KiB"), "{frame}");
    // A re-sort lays out again.
    app.key(press(KeyCode::Char('S')), t0);
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
    app.key(press(KeyCode::Char(' ')), t0);
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
    let es: Vec<EntryData> = (0..10_000u32)
        .map(|i| {
            test_entry(
                i,
                &format!("entry-{i}"),
                1 + (u64::from(i) * 7919) % 100_000,
            )
        })
        .collect();
    let app = map_of(es, t0);
    let area = Rect::new(0, 3, 200, 54);
    let mut best = Duration::MAX;
    for _ in 0..5 {
        let t = Instant::now();
        let l = build(&app, area, false);
        best = best.min(t.elapsed());
        assert!(l.tiles.iter().any(|t| t.small > 0));
    }
    let bound = if cfg!(debug_assertions) { 40 } else { 2 };
    assert!(best.as_millis() < bound, "{best:?}");
}

#[test]
fn category_color_hatches_cleanup_candidates() {
    let fx = classified();
    let t0 = Instant::now();
    let mut app = treemap_app(&fx.m, t0);
    // Category is the default, and the legend names the categories on screen.
    let frame = render(&mut app, t0, 100, 30);
    assert!(frame.contains("colour: category  ■ build"), "{frame}");
    assert!(frame.contains("╱ cleanup candidate"), "{frame}");
    insta::assert_snapshot!("treemap_category_100x30", frame);
    // `p` steps through age and top folder, and back.
    for want in ["age", "top folder", "category"] {
        app.key(press(KeyCode::Char('p')), t0);
        let frame = render(&mut app, t0, 100, 30);
        assert!(frame.contains(&format!("colour: {want}")), "{frame}");
    }
}

/// In colour, tiles are fills without frames: only the selected tile has an edge, its header
/// row in the focus colour, and every other tile's header is a shade of its own fill.
#[test]
fn colour_fills_tiles_and_outlines_only_the_selection() {
    let fx = classified();
    let t0 = Instant::now();
    let mut app = treemap_app(&fx.m, t0);
    app.theme = Theme::named("dark", crate::theme::ColorLevel::TrueColor, None).expect("dark");
    let buf = render_buffer(&mut app, t0, 80, 18);
    let l = app.treemap.layout.as_ref().expect("layout");
    let mut edge = 0;
    for p in l.area.positions() {
        let s = buf[p].symbol();
        assert!(
            !["┌", "─", "│", "┐", "└", "┘"].contains(&s),
            "a frame at {p:?}"
        );
        edge += usize::from(["┃", "━", "┗", "┛"].contains(&s));
    }
    assert!(edge > 0, "the selection has an edge");
    let big = l.tiles[l.tops[0]].rect;
    assert_eq!(Some(buf[(big.x, big.y)].bg), app.theme.border_focus.fg);
    let other = l.tiles[l.tops[1]].rect;
    let (head, body) = (buf[(other.x, other.y)].bg, buf[(other.x, other.y + 1)].bg);
    assert!(matches!(head, ratatui::style::Color::Rgb(..)), "{head:?}");
    assert_ne!(head, body, "the header is a darker shade");
    insta::assert_snapshot!("treemap_truecolor_80x18", styled(&buf));
}

/// At depth 1 only the selected tile shows what is in it, and moving the selection moves that.
#[test]
fn depth_one_peeks_into_the_selected_tile() {
    let fx = tree();
    let t0 = Instant::now();
    let mut app = treemap_app(&fx.m, t0);
    assert_eq!(app.treemap.depth, 1);
    let frame = render(&mut app, t0, 100, 30);
    assert_eq!(selected(&app), "big");
    assert!(frame.contains(" a/") && !frame.contains(" m0"), "{frame}");
    let mid = app
        .list
        .rows
        .iter()
        .position(|&i| &*app.snap.entries[i].name == b"mid");
    app.move_to(mid.expect("mid is a row"));
    assert_eq!(selected(&app), "mid");
    let frame = render(&mut app, t0, 100, 30);
    assert!(!frame.contains(" a/") && frame.contains(" m0"), "{frame}");
    // Depth 2 shows them in every tile.
    app.key(press(KeyCode::Char('+')), t0);
    sync(&mut app, &fx.m, t0);
    let frame = render(&mut app, t0, 100, 30);
    assert!(frame.contains(" a/") && frame.contains(" m0"), "{frame}");
}

#[test]
fn list_and_details_show_the_category() {
    let fx = classified();
    let t0 = Instant::now();
    let mut app = App::new(Theme::no_color(), t0);
    crate::features::details::toggle(&mut app);
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
    app.key(press(KeyCode::Down), t0);
    sync(&mut app, &fx.m, t0);
    let frame = render(&mut app, t0, 120, 24);
    assert!(frame.contains("by category"), "{frame}");
    assert!(frame.contains("OS and user caches"), "{frame}");
}

/// The list and the treemap share one selection: switching views keeps the selected entry.
#[test]
fn switching_views_keeps_the_selection() {
    let fx = tree();
    let t0 = Instant::now();
    let mut app = App::new(Theme::no_color(), t0);
    sync(&mut app, &fx.m, t0);
    render(&mut app, t0, 100, 30);
    app.key(press(KeyCode::Down), t0);
    assert_eq!(selected(&app), "mid");
    app.key(press(KeyCode::Char('2')), t0);
    sync(&mut app, &fx.m, t0);
    let buf = render_buffer(&mut app, t0, 100, 30);
    assert_eq!(selected(&app), "mid");
    let l = app.treemap.layout.as_ref().expect("layout");
    let top = l.tops[selected_top(&app).expect("a selected tile")];
    assert_eq!(l.tiles[top].name, "mid");
    let r = l.tiles[top].rect;
    assert_eq!(
        buf[(r.x, r.y)].symbol(),
        "┏",
        "the selected tile has the heavy frame"
    );

    app.key(press(KeyCode::Char('j')), t0);
    let moved = selected(&app);
    assert_ne!(moved, "mid");
    app.key(press(KeyCode::Char('1')), t0);
    sync(&mut app, &fx.m, t0);
    render(&mut app, t0, 100, 30);
    assert_eq!(selected(&app), moved);
    app.key(press(KeyCode::Char('2')), t0);
    sync(&mut app, &fx.m, t0);
    render(&mut app, t0, 100, 30);
    assert_eq!(selected(&app), moved);
}

/// Names that do not fit are left out rather than cut to a stub (`dir-1…`), the merged tiles
/// say how many entries they hold only when that fits whole, and slivers at the edges are left
/// blank.
#[test]
fn narrow_tiles_show_short_labels_or_none() {
    let (mut fx, root) = Fixture::new();
    fx.m.tree.dir_mut(root).expect("root").state = DirState::Complete;
    for i in 0..30u64 {
        let d = fx.dir(root, &format!("dir-{i}"));
        for j in 0..20u64 {
            fx.file(d, &format!("f{j}.bin"), (40 - i) * 1000 + j * 700);
        }
    }
    let t0 = Instant::now();
    let mut app = nested_app(&fx.m, t0);
    for (w, h) in [(80, 24), (100, 30)] {
        let buf = render_buffer(&mut app, t0, w, h);
        let frame = render(&mut app, t0, w, h);
        for cut in ["dir-1…", "dir-…", "19 m…", "19 mo…", "19 more…", "19 …"] {
            assert!(!frame.contains(cut), "{w}x{h} has {cut:?}:\n{frame}");
        }
        let l = app.treemap.layout.as_ref().expect("layout");
        let slivers: Vec<&Tile> = l.tiles.iter().filter(|t| t.rect.width < 3).collect();
        assert!(!slivers.is_empty(), "{w}x{h}: the fixture has slivers");
        for t in slivers {
            for (x, y) in t.rect.positions().map(|p| (p.x, p.y)) {
                assert_eq!(buf[(x, y)].symbol(), " ", "{w}x{h}: {:?}\n{frame}", t.name);
            }
        }
    }
    insta::assert_snapshot!("treemap_narrow_tiles_80x24", render(&mut app, t0, 80, 24));
}

/// With ASCII bars, the list's bars are `#` and the treemap's frames `=`, `|` and `+`: no block
/// or box-drawing characters.
#[test]
fn ascii_bars_draw_without_block_characters() {
    let fx = tree();
    let t0 = Instant::now();
    let mut app = App::new(Theme::no_color(), t0);
    app.theme.bars = crate::theme::Bars::Ascii;
    sync(&mut app, &fx.m, t0);
    let list = render(&mut app, t0, 100, 30);
    assert!(list.contains("#####"), "{list}");
    let blocks = ['█', '▏', '▎', '▍', '▌', '▋', '▊', '▉', '░'];
    assert!(!list.contains(blocks), "{list}");
    app.key(press(KeyCode::Char('2')), t0);
    sync(&mut app, &fx.m, t0);
    let map = render(&mut app, t0, 100, 30);
    assert!(map.contains("+====") && map.contains("#####"), "{map}");
    assert!(!map.contains(blocks), "{map}");
    assert!(!map.contains(['─', '│', '┌', '━', '┃', '┏']), "{map}");
    insta::assert_snapshot!("treemap_ascii_100x30", map);
}
