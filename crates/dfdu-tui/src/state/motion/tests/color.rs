use ratatui::crossterm::event::KeyCode;
use ratatui::style::{Color, Modifier};

use super::*;
use crate::tests::{press, style_at, styled};

fn has_rgb(buf: &Buffer) -> bool {
    buf.content.iter().any(|c| {
        [c.fg, c.bg]
            .into_iter()
            .any(|c| matches!(c, Color::Rgb(..)))
    })
}

/// Themes, and whether the colour effects run for them: only at `TrueColor`, on a background that
/// is known or that the theme's RGB text colour was chosen for.
fn color_themes() -> [(Theme, bool); 8] {
    let truecolor = |name, background| {
        Theme::named(name, ColorLevel::TrueColor, background).expect("built-in theme")
    };
    [
        (Theme::ansi16(), false),
        (Theme::no_color(), false),
        (dark(ColorLevel::Ansi256), false),
        (dark(ColorLevel::TrueColor), true),
        // The terminal's own text colour, on a background that did not answer the query.
        (truecolor("ansi", None), false),
        (truecolor("high-contrast", None), false),
        (truecolor("ansi", Some((250, 250, 250))), true),
        (truecolor("high-contrast", Some((0, 0, 0))), true),
    ]
}

#[test]
fn delete_fade_only_colors_at_truecolor() {
    for (theme, truecolor) in color_themes() {
        let name = theme.name().to_owned();
        assert_eq!(effects::rgb_effects(&theme), truecolor, "{name}");
        // A theme with neither a dim nor a text colour has nothing to fade from: the row keeps
        // the terminal's text colour, which the background never hides.
        let fades = truecolor && theme.fg_dim.fg.or(theme.fg.fg).is_some();
        let dim = theme.fg_dim.fg;
        let hidden = theme.background().map(|(r, g, b)| Color::Rgb(r, g, b));
        let t0 = Instant::now();
        let mut app = animated(theme, t0);
        let b = EntryData {
            mark: Mark::Deleting,
            ..test_entry(1, "b", 200)
        };
        app.refresh(snap_with_alloc(vec![test_entry(0, "a", 300), b], 1), t0);
        render_buffer(&mut app, t0, W, H);
        let t1 = t0 + ms(100);
        app.refresh(snap_with_alloc(vec![test_entry(0, "a", 300)], 2), t1);
        let mut fgs = Vec::new();
        for n in 0..=6 {
            let buf = render_buffer(&mut app, t1 + ms(n * 50), W, H);
            if !truecolor {
                assert!(!has_rgb(&buf), "{}", styled(&buf));
            }
            fgs.push(style_at(&buf, "b  deleted").fg);
        }
        assert!(
            hidden.is_none() || fgs.iter().all(|&c| c != hidden),
            "{name}: the row fades into the background {fgs:?}"
        );
        if fades {
            assert_eq!(fgs[0], dim);
            assert_ne!(fgs[2], fgs[0]);
            assert_ne!(fgs[5], fgs[2]);
            // Done after 250 ms: the row stays faded.
            assert_eq!(fgs[5], fgs[6]);
        } else {
            assert!(fgs.iter().all(|&c| c == fgs[0]), "{fgs:?}");
        }
        assert_eq!(app.motion.next_frame_at(), None);
    }
}

/// Draws a frame at `now`, then each frame the motion asks for before `until`, and records the
/// start of each flash in `flashes`.
fn note_flashes(app: &mut App, mut now: Instant, until: Instant, flashes: &mut Vec<Instant>) {
    loop {
        render_buffer(app, now, W, H);
        if let Some(t) = app.motion.flash_at
            && flashes.last() != Some(&t)
        {
            flashes.push(t);
        }
        match app.motion.next_frame_at() {
            Some(t) if t < until => now = t,
            _ => return,
        }
    }
}

#[test]
fn flashes_are_rate_limited() {
    let t0 = Instant::now();
    let mut app = animated(dark(ColorLevel::TrueColor), t0);
    let mut es = resort_fixture(&mut app, t0);
    let mut flashes = Vec::new();

    let t1 = t0 + Duration::from_secs(3);
    app.refresh(snap_with_alloc(es.clone(), 2), t1);
    let t2 = t1 + Duration::from_secs(2);
    note_flashes(&mut app, t1, t2, &mut flashes);
    assert_eq!(flashes, [t1]);

    // The next automatic re-sort, 2 s later.
    es[1].size = 5000;
    app.refresh(snap_with_alloc(es.clone(), 3), t2);
    assert_eq!(row_names(&app), ["b", "c", "a"]);
    let t3 = t2 + ms(100);
    note_flashes(&mut app, t2, t3, &mut flashes);
    // The scan finishes 100 ms later: a re-sort at once, with markers but no flash.
    es[0].size = 9000;
    let mut done = snap_with_alloc(es, 4);
    done.busy = false;
    app.refresh(done, t3);
    assert_eq!(row_names(&app), ["a", "b", "c"]);
    note_flashes(&mut app, t3, t3 + Duration::from_secs(5), &mut flashes);
    assert!(app.motion.next_frame_at().is_none());
    assert_eq!(flashes, [t1, t2]);
}

#[test]
fn flash_colors_only_at_truecolor() {
    for (theme, truecolor) in color_themes() {
        let name = theme.name().to_owned();
        let highlight = theme.cursor.add_modifier.contains(Modifier::REVERSED);
        let t0 = Instant::now();
        let mut app = animated(theme, t0);
        let es = resort_fixture(&mut app, t0);
        let t1 = t0 + Duration::from_secs(3);
        app.refresh(snap_with_alloc(es, 2), t1);
        assert_eq!(row_names(&app), ["c", "a", "b"]);
        assert_eq!(app.selected_index(), Some(0), "the top row stays selected");
        let buf = render_buffer(&mut app, t1, W, H);
        assert_eq!(has_rgb(&buf), truecolor, "{name}\n{}", styled(&buf));
        let later = render_buffer(&mut app, t1 + FLASH, W, H);
        let reversed =
            |buf: &Buffer, row| style_at(buf, row).add_modifier.contains(Modifier::REVERSED);
        let (during, after) = (style_at(&buf, "a  ↓1"), style_at(&later, "a  ↓1"));
        if truecolor {
            assert!(
                matches!(during.bg, Some(Color::Rgb(..))),
                "{name} {during:?}"
            );
            assert_ne!(during.bg, after.bg, "{name}");
        } else {
            assert_ne!(reversed(&buf, "a  ↓1"), reversed(&later, "a  ↓1"), "{name}");
            // The selected row keeps its highlight through the flash.
            assert_eq!(reversed(&buf, "c  ↑2"), highlight, "{name}");
            assert_eq!(reversed(&later, "c  ↑2"), highlight, "{name}");
        }
    }
}

#[test]
fn effects_skip_the_overlay() {
    effects_skip(
        |app, t| {
            app.key(press(KeyCode::Char('?')), t);
        },
        |app| app.areas.overlay,
    );
}

#[test]
fn effects_skip_the_debug_panel() {
    effects_skip(
        |app, _| crate::features::debug::toggle(app),
        |app| app.areas.debug,
    );
}

/// Every row flashes after a re-sort, with something `open` drawn over the list at `cover`:
/// the cells it covers are the same as without the animations.
fn effects_skip(open: impl Fn(&mut App, Instant), cover: impl Fn(&App) -> Option<Rect>) {
    let (w, h) = (100, 30);
    let t0 = Instant::now();
    let es: Vec<EntryData> = (0..30)
        .map(|i| test_entry(i, &format!("e{i:02}"), 1000 + u64::from(i) * 10))
        .collect();
    // Every row moves in the re-sort, so every row flashes.
    let reversed: Vec<EntryData> = es
        .iter()
        .map(|e| EntryData {
            size: 5000 - e.size,
            ..e.clone()
        })
        .collect();
    let mut frames = Vec::new();
    for on in [false, true] {
        let mut app = App::new(dark(ColorLevel::TrueColor), t0);
        if on {
            start(&mut app);
        }
        app.refresh(snap_with_alloc(es.clone(), 1), t0);
        render_buffer(&mut app, t0, w, h);
        let t1 = t0 + Duration::from_secs(3);
        app.refresh(snap_with_alloc(reversed.clone(), 2), t1);
        open(&mut app, t1);
        frames.push(render_buffer(&mut app, t1, w, h));
        if on {
            assert!(app.motion.flash_at.is_some());
            let cover = cover(&app).expect("drawn");
            let list = app.areas.list;
            let (plain, fx) = (&frames[0], &frames[1]);
            for y in list.top()..list.bottom() {
                for x in list.left()..list.right() {
                    if cover.contains((x, y).into()) {
                        assert_eq!(plain[(x, y)], fx[(x, y)], "({x}, {y})");
                    }
                }
            }
            // Left of it the rows flash.
            let y = cover.top().max(list.top()) + 1;
            assert_ne!(plain[(0, y)].bg, fx[(0, y)].bg);
        }
    }
}
