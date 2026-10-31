//! Every main state of the browser in the built-in themes for dark, light, 16-colour and
//! `NO_COLOR` terminals, as snapshots of the text and the cell styles; and checks that no colour
//! carries a meaning alone: the text is the same in every theme, and each cue has a glyph, a
//! word or a modifier without colours.

use std::time::Instant;

use dfdu_engine::{ErrorItem, ErrorList, SizeMode};
use dfdu_scan::Budget;
use ratatui::buffer::Buffer;
use ratatui::crossterm::event::KeyCode;
use ratatui::style::{Color, Modifier, Style};

use super::{
    assert_cue, mid_scan_app, press, render_buffer, shared_model, style_at, styled, text, typed,
};
use crate::app::{App, Effect, Overlay};
use crate::features::debug::Counters;
use crate::features::details;
use crate::format::spinner;
use crate::model::extras;
use crate::model::snapshot::{self, EntryData, Mark, Target, test_entry};
use crate::state::filter;
use crate::theme::{BuiltinTheme, ColorLevel, Theme, choose_theme, detect_color_level};

/// The built-in themes `auto` picks for a dark or light truecolor terminal, a 16-colour one
/// and `NO_COLOR`, each at that colour level.
const THEMES: [(&str, ColorLevel); 4] = [
    ("dark", ColorLevel::TrueColor),
    ("light", ColorLevel::TrueColor),
    ("ansi", ColorLevel::Ansi16),
    ("mono", ColorLevel::None),
];

const WIDTH: u16 = 100;
const HEIGHT: u16 = 24;

#[derive(Copy, Clone, Debug)]
enum State {
    MidScan,
    Details,
    Delete,
    Filter,
    Help,
    Palette,
    Errors,
    Debug,
}

impl State {
    const ALL: [State; 8] = [
        State::MidScan,
        State::Details,
        State::Delete,
        State::Filter,
        State::Help,
        State::Palette,
        State::Errors,
        State::Debug,
    ];

    fn name(self) -> &'static str {
        match self {
            State::MidScan => "mid_scan",
            State::Details => "details",
            State::Delete => "delete",
            State::Filter => "filter",
            State::Help => "help",
            State::Palette => "palette",
            State::Errors => "errors",
            State::Debug => "debug",
        }
    }
}

fn theme(name: &str, level: ColorLevel) -> Theme {
    Theme::named(name, level, None).expect("built-in theme")
}

fn mono() -> Theme {
    theme("mono", ColorLevel::None)
}

fn press_all(app: &mut App, now: Instant, codes: &[KeyCode]) -> Vec<Effect> {
    codes.iter().flat_map(|&c| app.key(press(c), now)).collect()
}

/// What the mid-scan home directory could not read.
fn error_list() -> ErrorList {
    let node = test_entry(4, "locked", 0).node;
    let item = |path: &str, is_dir, problem: &str| ErrorItem {
        node,
        path: path.into(),
        is_dir,
        problem: problem.into(),
    };
    ErrorList {
        root: "/home/jo".into(),
        items: vec![
            item("/home/jo/locked", true, "permission denied"),
            item("/home/jo/dev/cache/index.lock", false, "permission denied"),
        ],
        total: 2,
        kinds: vec![(dfdu_tree::ErrKind::PermissionDenied, 2)],
    }
}

/// The browser on the finished scan of [`shared_model`], on its first file, with the details
/// pane open and read.
fn finished_with_details(theme: Theme) -> (App, Instant) {
    let m = shared_model();
    let t0 = Instant::now();
    let mut app = App::new(theme, t0);
    app.refresh(snapshot::build(&m, &mut Target::root(), SizeMode::Disk), t0);
    press_all(&mut app, t0, &[KeyCode::Down, KeyCode::Char('i')]);
    let x = extras::read(&m, &app, &Budget::new(1 << 20), Counters::default());
    extras::apply(&mut app, x);
    (app, t0)
}

/// The frame of `state` in `theme`, reached through the keys a user presses.
fn frame(state: State, theme: Theme) -> Buffer {
    if let State::Details = state {
        let (mut app, now) = finished_with_details(theme);
        // The first frame lays the pane out, so that Tab can focus it.
        render_buffer(&mut app, now, WIDTH, HEIGHT);
        press_all(&mut app, now, &[KeyCode::Tab]);
        assert!(details::focused(&app), "the pane has the focus");
        assert!(!app.snap.busy && app.extras.details.facts.is_some());
        return render_buffer(&mut app, now, WIDTH, HEIGHT);
    }
    let (mut app, now) = mid_scan_app(theme);
    let c = KeyCode::Char;
    match state {
        State::MidScan | State::Details => {}
        // Library and .android marked, then the dialog for both.
        State::Delete => {
            press_all(&mut app, now, &[c(' '), KeyCode::Down, c(' '), c('d')]);
        }
        State::Filter => typed(&mut app, "/li", now),
        State::Help => typed(&mut app, "?", now),
        State::Palette => typed(&mut app, ":sort", now),
        State::Errors => {
            let fx = press_all(&mut app, now, &[c('!')]);
            assert!(matches!(fx[..], [Effect::ShowErrors]));
            app.show_errors(error_list());
        }
        State::Debug => {
            press_all(&mut app, now, &[KeyCode::F(12)]);
        }
    }
    let reached = match state {
        State::MidScan => app.snap.busy && matches!(app.overlay, Overlay::None),
        State::Details => unreachable!("returned above"),
        State::Delete => {
            app.marks.items().len() == 2 && matches!(app.overlay, Overlay::ConfirmDelete(_))
        }
        State::Filter => filter::editing(&app) && app.rows.len() == 2,
        State::Help => matches!(app.overlay, Overlay::Help(_)),
        State::Palette => matches!(app.overlay, Overlay::Palette(_)),
        State::Errors => matches!(app.overlay, Overlay::Errors { .. }),
        State::Debug => app.debug.open && matches!(app.overlay, Overlay::None),
    };
    assert!(reached, "{} was not reached", state.name());
    render_buffer(&mut app, now, WIDTH, HEIGHT)
}

fn matrix(state: State) {
    for (name, level) in THEMES {
        let snapshot = format!("theme_matrix_{}_{name}", state.name());
        insta::assert_snapshot!(snapshot, styled(&frame(state, theme(name, level))));
    }
}

#[test]
fn theme_matrix_mid_scan() {
    matrix(State::MidScan);
}

#[test]
fn theme_matrix_details() {
    matrix(State::Details);
}

#[test]
fn theme_matrix_delete() {
    matrix(State::Delete);
}

#[test]
fn theme_matrix_filter() {
    matrix(State::Filter);
}

#[test]
fn theme_matrix_help() {
    matrix(State::Help);
}

#[test]
fn theme_matrix_palette() {
    matrix(State::Palette);
}

#[test]
fn theme_matrix_errors() {
    matrix(State::Errors);
}

#[test]
fn theme_matrix_debug() {
    matrix(State::Debug);
}

#[test]
fn text_layer_is_identical_across_themes() {
    for state in State::ALL {
        let mono = text(&frame(state, mono()));
        for (name, level) in THEMES {
            let themed = text(&frame(state, theme(name, level)));
            assert_eq!(themed, mono, "{} in {name}", state.name());
        }
    }
}

#[test]
fn mono_frames_have_no_colors() {
    let detected = detect_color_level(None, |k| (k == "NO_COLOR").then(|| "1".into()));
    assert_eq!(detected.level, ColorLevel::None);
    let chosen = choose_theme(None, detected.level, None);
    assert_eq!(chosen, BuiltinTheme::Mono, "NO_COLOR selects mono");
    assert_eq!(Theme::builtin(chosen, detected.level, None), mono());
    for state in State::ALL {
        let buf = frame(state, mono());
        for (i, cell) in buf.content.iter().enumerate() {
            let at = (i % usize::from(WIDTH), i / usize::from(WIDTH));
            assert_eq!(
                (cell.fg, cell.bg),
                (Color::Reset, Color::Reset),
                "{} at {at:?}\n{}",
                state.name(),
                styled(&buf)
            );
        }
    }
}

/// The glyph left of the size and the words after the name that tell `mark` in the list.
fn cue(mark: Mark) -> (&'static [char], &'static str) {
    const SPINNER: [char; 10] = ['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏'];
    match mark {
        Mark::None => (&[' '], ""),
        Mark::Scanning => (&SPINNER, ""),
        Mark::Stalled => (&['~'], "not responding"),
        Mark::Rescanning => (&SPINNER, "rescanning"),
        Mark::Error => (&['!'], "incomplete"),
        Mark::OtherFs => (&['>'], "other filesystem: m scans it"),
        Mark::DupMount => (&['='], "mounted again: counted at its other mount"),
        Mark::ErrorsInside => (&['.'], "2 errors inside"),
        Mark::Network => (&['>'], "network filesystem: m scans it"),
        Mark::Kernel => (&['>'], "kernel filesystem"),
        Mark::Deleting => (&['x'], "deleting"),
        Mark::Incomplete => (&['?'], "incomplete: r to rescan"),
    }
}

/// The size shown is still growing, or only a lower bound.
fn incomplete(mark: Mark) -> bool {
    matches!(
        mark,
        Mark::Scanning
            | Mark::Rescanning
            | Mark::Stalled
            | Mark::Incomplete
            | Mark::Error
            | Mark::ErrorsInside
    )
}

const MARKS: [Mark; 12] = [
    Mark::None,
    Mark::Scanning,
    Mark::Stalled,
    Mark::Rescanning,
    Mark::Error,
    Mark::OtherFs,
    Mark::DupMount,
    Mark::ErrorsInside,
    Mark::Network,
    Mark::Kernel,
    Mark::Deleting,
    Mark::Incomplete,
];

/// The row of the frame that holds `needle`.
#[track_caller]
fn row_with(buf: &Buffer, needle: &str) -> (u16, String) {
    let frame = text(buf);
    let (y, line) = frame
        .lines()
        .enumerate()
        .find(|(_, l)| l.contains(needle))
        .unwrap_or_else(|| panic!("{needle:?} is not in the frame:\n{frame}"));
    (u16::try_from(y).unwrap(), line.to_owned())
}

#[test]
fn every_mark_has_a_glyph() {
    let t0 = Instant::now();
    let es: Vec<EntryData> = MARKS
        .iter()
        .enumerate()
        .map(|(i, &mark)| EntryData {
            is_dir: true,
            items: 3,
            mark,
            errors_inside: if mark == Mark::ErrorsInside { 2 } else { 0 },
            ..test_entry(u32::try_from(i).unwrap(), &format!("entry-{i:02}"), 1 << 20)
        })
        .collect();
    let mut app = App::new(mono(), t0);
    app.refresh(crate::tests::snap_with_totals(es, 1), t0);
    let buf = render_buffer(&mut app, t0, 140, 20);
    let mut seen = Vec::new();
    for (i, &mark) in MARKS.iter().enumerate() {
        let name = format!("entry-{i:02}/");
        let (y, row) = row_with(&buf, &name);
        let (glyphs, words) = cue(mark);
        let glyph = row.chars().nth(1).expect("a glyph column");
        assert!(glyphs.contains(&glyph), "{mark:?}: {row}");
        let after = row.split_once(&name).expect("the name").1;
        assert_eq!(after.trim(), words, "{mark:?}: {row}");
        let shown = cue(mark);
        assert!(
            !seen.contains(&shown),
            "{mark:?} looks like another mark: {row}"
        );
        seen.push(shown);
        // The size cells: dim while the size can still grow or is a lower bound.
        for x in 2..12 {
            let cell = &buf[(x, y)];
            if cell.symbol() != " " {
                let dim = cell.modifier.contains(Modifier::DIM);
                assert_eq!(dim, incomplete(mark), "{mark:?}: size at column {x}: {row}");
            }
        }
    }
    let spinners: Vec<char> = (0..10).map(spinner).collect();
    assert_eq!(cue(Mark::Scanning).0, &spinners[..]);

    let reversed = |s: Style| s.add_modifier.contains(Modifier::REVERSED);
    let first = format!("entry-{:02}", app.rows[0]);
    assert_cue(&buf, &first, reversed);
    assert!(!reversed(style_at(
        &buf,
        &format!("entry-{:02}", app.rows[1])
    )));

    // A marked row starts with `●`, and so does the count of marks.
    press_all(&mut app, t0, &[KeyCode::Char(' ')]);
    let buf = render_buffer(&mut app, t0, 140, 20);
    let (_, row) = row_with(&buf, &format!("{first}/"));
    assert!(row.starts_with('●'), "{row}");
    assert_cue(&buf, "●", |s| s.add_modifier.contains(Modifier::BOLD));
    assert!(text(&buf).contains("● 1 marked"));

    // Matched characters of the filter are underlined.
    typed(&mut app, "/y-07", t0);
    let buf = render_buffer(&mut app, t0, 140, 20);
    assert_cue(&buf, "y-07", |s| {
        s.add_modifier.contains(Modifier::UNDERLINED)
    });
    assert!(
        !style_at(&buf, "entr")
            .add_modifier
            .contains(Modifier::UNDERLINED)
    );

    // The details pane has a heavy border while it has the focus, a rounded one otherwise.
    let (mut app, now) = finished_with_details(mono());
    let buf = render_buffer(&mut app, now, WIDTH, HEIGHT);
    assert!(text(&buf).contains('╭') && !text(&buf).contains('┏'));
    press_all(&mut app, now, &[KeyCode::Tab]);
    let buf = render_buffer(&mut app, now, WIDTH, HEIGHT);
    assert!(!text(&buf).contains('╭'));
    assert_cue(&buf, "┏", |s| s.add_modifier.contains(Modifier::BOLD));
}
