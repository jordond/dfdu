use std::fmt::Write as _;

use ratatui::buffer::Buffer;
use ratatui::crossterm::event::KeyCode;
use ratatui::style::Modifier;

use super::*;
use crate::model::snapshot::Snapshot;
use crate::tests::{below, press, render_buffer, snap_without_totals, style_at, text};
use crate::theme::Theme;

/// A browser of `dir` below the scan root `root`, drawn `width` columns wide.
fn browser(dir: &str, root: &str, theme: Theme, width: u16) -> (App, Buffer) {
    let t0 = Instant::now();
    let mut app = App::new(theme, t0);
    let below = Snapshot {
        dir_path: dir.into(),
        root_path: root.into(),
        at_root: dir == root,
        ..snap_without_totals(Vec::new(), 1)
    };
    app.refresh(below, t0);
    let buf = render_buffer(&mut app, t0, width, 8);
    (app, buf)
}

fn first_line(buf: &Buffer) -> String {
    text(buf).lines().next().unwrap_or_default().to_owned()
}

/// The scope line: the second line of the header.
fn scope_line(buf: &Buffer) -> String {
    text(buf).lines().nth(1).unwrap_or_default().to_owned()
}

#[test]
fn breadcrumbs_show_path_below_root() {
    let (app, buf) = browser("/srv/data/dev/rust", "/srv/data", Theme::ansi16(), 100);
    let scope = scope_line(&buf);
    assert!(scope.starts_with(" /srv/data › dev › rust "), "{scope}");
    // The current directory is styled as the title, the way to it dimmed.
    assert_eq!(style_at(&buf, "rust"), style_at(&buf, "dfdu"));
    assert_eq!(style_at(&buf, "dev"), style_at(&buf, "›"));
    assert_ne!(style_at(&buf, "dev"), style_at(&buf, "rust"));
    let children: Vec<_> = app.mouse.crumbs.iter().map(|c| c.child.clone()).collect();
    assert_eq!(
        children,
        [below("/srv/data", "dev"), below("/srv/data", "dev/rust")]
    );
}

#[test]
fn long_paths_collapse_from_the_left() {
    let dir = "/srv/data/projects/rust/dfdu/src/ui";
    let (app, buf) = browser(dir, "/srv/data", Theme::no_color(), 48);
    let header: String = text(&buf).lines().take(2).flat_map(|l| [l, "\n"]).collect();
    insta::assert_snapshot!("breadcrumbs_collapsed", header);
    let scope = scope_line(&buf);
    assert!(scope.starts_with(" … › src › ui "), "{scope}");
    let crumbs: Vec<_> = app
        .mouse
        .crumbs
        .iter()
        .map(|c| (c.area, c.child.clone()))
        .collect();
    assert_eq!(
        crumbs,
        [(
            Rect::new(5, 1, 3, 1),
            below("/srv/data", "projects/rust/dfdu/src/ui")
        )]
    );

    let (_, buf) = browser(dir, "/srv/data", Theme::no_color(), 100);
    let scope = scope_line(&buf);
    assert!(
        scope.starts_with(" /srv/data › projects › rust › dfdu › src › ui "),
        "{scope}"
    );
}

/// The header of a read-only browser at the scan root `root`, done scanning, drawn `width`
/// columns wide: the tab bar and the scope line.
fn done_at_root(root: &str, width: u16) -> (String, String) {
    let t0 = Instant::now();
    let mut app = App::new(Theme::no_color(), t0);
    app.read_only = true;
    let snap = Snapshot {
        dir_path: root.into(),
        root_path: root.into(),
        busy: false,
        ..snap_without_totals(Vec::new(), 1)
    };
    app.refresh(snap, t0);
    let buf = render_buffer(&mut app, t0, width, 8);
    (first_line(&buf), scope_line(&buf))
}

#[test]
fn the_scan_state_shortens_before_the_tabs_do() {
    let root = "/srv/archive";
    let at = |width| done_at_root(root, width).0;
    let wide = at(100);
    assert!(wide.contains("│ 5 Largest "), "{wide}");
    assert!(wide.ends_with(" read-only · done in 00:00"), "{wide}");
    // Then the state gives way to the named tabs...
    let first = at(80);
    assert!(first.contains("│ 5 Largest"), "{first}");
    assert!(first.ends_with(" done 00:00"), "{first}");
    assert!(!first.contains("read-only"), "{first}");
    // ...and below 80 columns only the view shown is named.
    let first = at(79);
    assert!(first.contains("│ [1 Files] 2 3 4 5 "), "{first}");
    assert!(first.ends_with(" read-only · done in 00:00"), "{first}");
    let first = at(32);
    assert!(first.ends_with(" 00:00"), "{first}");
    // The last cell, a space, is trimmed.
    assert_eq!(Span::raw(&first).width(), 31, "{first}");
}

/// A long scan root is shortened in the middle before the size shown gives way.
#[test]
fn a_long_root_leaves_room_for_the_size_shown() {
    let root = "/private/tmp/claude-501/scratchpad/projects/2026/client-deliverables/demo";
    let (_, scope) = done_at_root(root, 100);
    assert!(scope.contains("size ↓ · disk"), "{scope}");
    assert!(scope.contains('…'), "{scope}");
}

#[test]
fn a_long_scan_root_is_shortened_in_the_middle() {
    let root = "/Volumes/Backup Drive/archive/projects/2026/client-deliverables/final";
    insta::assert_snapshot!("long_root_70", done_at_root(root, 70).1);
    insta::assert_snapshot!("long_root_50", done_at_root(root, 50).1);
    // Below the root, the current directory keeps its name and the root collapses first.
    let (_, buf) = browser(&format!("{root}/renders"), root, Theme::no_color(), 50);
    let scope = scope_line(&buf);
    assert!(scope.starts_with(" … › renders "), "{scope}");
    assert!(first_line(&buf).contains("scanning 00:00"), "{buf:?}");
}

#[test]
fn header_shows_the_home_directory_as_tilde() {
    let t0 = Instant::now();
    let mut app = App::new(Theme::no_color(), t0);
    let below = Snapshot {
        dir_path: "/home/jo/dev".into(),
        root_path: "/home/jo".into(),
        at_root: false,
        ..snap_without_totals(Vec::new(), 1)
    };
    app.refresh(below, t0);
    let scope = scope_line(&render_buffer(&mut app, t0, 80, 8));
    assert!(scope.starts_with(" /home/jo › dev "), "{scope}");

    app.home = Some("/home/jo".into());
    let scope = scope_line(&render_buffer(&mut app, t0, 80, 8));
    assert!(scope.starts_with(" ~ › dev "), "{scope}");
    let crumbs: Vec<_> = app.mouse.crumbs.iter().map(|c| c.child.clone()).collect();
    assert_eq!(crumbs, [crate::tests::below("/home/jo", "dev")]);
}

/// The widths of the mockups, and a narrow one.
pub(super) const WIDTHS: [u16; 4] = [120, 80, 64, 48];

/// The two header lines of every view at each of [`WIDTHS`], in a directory below the root of
/// the classified tree of the treemap tests (so Cleanup has rules), one block per view.
#[test]
fn every_view_has_its_tab_and_scope_at_each_width() {
    use crate::app::views::{self, View};
    use crate::tests::{classified_app, sync};
    let t0 = Instant::now();
    let (fx, mut app) = classified_app(t0);
    let mid = app
        .list
        .rows
        .iter()
        .position(|&i| &*app.snap.entries[i].name == b"mid");
    app.select(mid.expect("mid is listed"));
    app.key(press(KeyCode::Enter), t0);
    sync(&mut app, &fx.m, t0);
    let mut out = String::new();
    for view in View::ALL {
        views::switch(&mut app, view);
        sync(&mut app, &fx.m, t0);
        assert_eq!(views::current(&app), view);
        writeln!(out, "{}:", view.name()).expect("write");
        for width in WIDTHS {
            let buf = render_buffer(&mut app, t0, width, 20);
            for line in text(&buf).lines().take(2) {
                writeln!(out, "{width:>3}|{line}").expect("write");
            }
        }
    }
    insta::assert_snapshot!("views_header", out);
    assert!(out.contains("whole scan: /r"), "{out}");
    assert!(out.contains(" /r › mid "), "{out}");
}

#[test]
fn the_active_tab_is_reversed_and_bold_in_every_theme() {
    for theme in [Theme::no_color(), Theme::ansi16()] {
        let (_, buf) = browser("/srv", "/srv", theme.clone(), 120);
        let active = style_at(&buf, " 1 Files ");
        assert!(
            active.add_modifier.contains(Modifier::REVERSED),
            "{active:?}"
        );
        assert!(active.add_modifier.contains(Modifier::BOLD), "{active:?}");
        let other = style_at(&buf, " 2 Map ");
        assert!(
            !other.add_modifier.contains(Modifier::REVERSED),
            "{other:?}"
        );
    }
}

#[test]
fn cleanup_shows_what_it_would_free_once_read() {
    use crate::app::views::{View, badge};
    use crate::tests::{classified_app, sync};
    let t0 = Instant::now();
    let (mut fx, mut app) = classified_app(t0);
    // Counted with the scan done, before the view was ever opened.
    let size = badge(&app, View::Cleanup).expect("a badge");
    let first = first_line(&render_buffer(&mut app, t0, 120, 8));
    assert!(first.contains(&format!("│ 3 Cleanup {size} │")), "{first}");
    // The same total as the view's summary bar.
    app.key(press(KeyCode::Char('3')), t0);
    sync(&mut app, &fx.m, t0);
    let frame = text(&render_buffer(&mut app, t0, 120, 20));
    assert!(frame.contains("Selected 0 B of 70.3 KiB"), "{frame}");
    assert_eq!(size, "70.3K");
    // With the view closed, the tab follows the tree: a match that grows.
    app.key(press(KeyCode::Char('1')), t0);
    let root = fx.m.tree.roots().next().expect("root");
    let big = (fx.m.tree.children(root))
        .find(|&c| fx.m.tree.name(c) == b"big")
        .expect("big");
    let meta = crate::tests::meta(dfdu_tree::Kind::File, 100 << 10, 900);
    let f = fx.m.tree.add_file(big, b"more", &meta, false, None);
    let own = fx.m.tree.own(f);
    fx.m.tree.propagate_add(big, &own);
    sync(&mut app, &fx.m, t0);
    assert_eq!(badge(&app, View::Cleanup).as_deref(), Some("170.3K"));
}

/// While the cached scan is read and then brought up to date, the scan state says so, with its
/// age, shorter on narrow terminals; then it is the usual one.
#[test]
fn a_cached_scan_says_so_until_it_is_refreshed() {
    use crate::model::cache_link::Cached;
    let root = "/srv/data";
    let t0 = Instant::now();
    let mut app = App::new(Theme::no_color(), t0);
    let snap = Snapshot {
        dir_path: root.into(),
        root_path: root.into(),
        busy: true,
        ..snap_without_totals(Vec::new(), 1)
    };
    app.refresh(snap, t0);
    app.cached = Some(Cached::Loading);
    let loading = first_line(&render_buffer(&mut app, t0, 100, 8));
    assert!(
        loading.ends_with("loading the cached scan 00:00"),
        "{loading}"
    );
    let written = dfdu_tree::now_secs().cast_unsigned() - 3 * 3_600 - 5;
    app.cached = Some(Cached::Refreshing { written });
    let header = |app: &mut App, width| {
        let buf = render_buffer(app, t0, width, 8);
        text(&buf)
            .lines()
            .take(2)
            .flat_map(|l| [l, "\n"])
            .collect::<String>()
    };
    insta::assert_snapshot!("cached_refreshing_100", header(&mut app, 100));
    let narrow = header(&mut app, 56);
    let first = narrow.lines().next().unwrap_or_default();
    assert!(first.ends_with(" cached 3 h ago"), "{narrow}");
    app.cached = None;
    let after = first_line(&render_buffer(&mut app, t0, 100, 8));
    assert!(after.ends_with("scanning 00:00"), "{after}");
}
