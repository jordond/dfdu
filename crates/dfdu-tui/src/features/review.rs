//! What the review views share: Largest, Duplicates and Cleanup all list candidates, let the
//! user tick some and delete what is ticked, so they look and behave alike.
//!
//! Ticks are the list's marks: ticking an item marks it, and the marks pane, the footer and the
//! delete dialog see it as any other mark. Each view starts with the summary bar
//! ([`summary`]): what is ticked against what it lists, and the key that deletes it. Rows start
//! with a checkbox ([`Tick`]) that reads without colour, give ages relative to now ([`age`]),
//! and shorten paths in the middle while keeping their last two components, with the part all
//! rows share dimmed ([`path`]). A view with a detail pane puts it on the right from
//! [`DETAIL_RIGHT_FROM`] columns and below the rows otherwise ([`split`]).
//!
//! The keys are the registry's: `Space` ticks, `A` ticks what the view suggests, `X` unticks
//! everything, `d` and `D` delete what is ticked; with nothing ticked they say how to tick
//! instead of deleting the list's selection hidden behind the view ([`delete_needs_ticks`]).

use ratatui::Frame;
use ratatui::crossterm::event::KeyEvent;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Padding, Paragraph, Wrap};

use crate::app::keys::{self, Action, first_key};
use crate::app::{App, Effect};
use crate::theme::{Theme, human, truncate_middle};

/// From this many columns the detail pane goes on the right of the rows; below, under them.
pub const DETAIL_RIGHT_FROM: u16 = 110;
/// The detail pane on the right takes this share of the width, within the bounds below.
const DETAIL_SHARE: u16 = 38;
const DETAIL_MIN_WIDTH: u16 = 36;
const DETAIL_MAX_WIDTH: u16 = 60;
/// Below the rows, the detail pane takes at most this many lines, and only when it leaves the
/// rows this many.
const DETAIL_MAX_HEIGHT: u16 = 6;
const ROWS_MIN: u16 = 8;
/// Cells of the checkbox column, its gap included.
pub const CHECK_WIDTH: usize = 4;
/// Before this (1990-01-01) a modification time is taken as unknown: an archive's zero, or a
/// clock that was never set.
const KNOWN_SINCE: i64 = 631_152_000;

/// What the summary bar counts: the size and number of the items ticked, of those listed.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub struct Tally {
    pub ticked_size: u64,
    pub size: u64,
    pub ticked: usize,
    pub items: usize,
}

impl Tally {
    /// Counts one item of `size`, ticked or not.
    pub fn add(&mut self, size: u64, ticked: bool) {
        self.size += size;
        self.items += 1;
        if ticked {
            self.ticked_size += size;
            self.ticked += 1;
        }
    }
}

/// The summary bar's style: the text colour on the subtle shade of an empty bar, bold without
/// colours.
pub fn bar_style(t: &Theme) -> Style {
    let mut s = Style::new().add_modifier(Modifier::BOLD);
    if let Some(fg) = t.fg.fg {
        s = s.fg(fg);
    }
    if let Some(bg) = t.bar_empty.fg {
        s = s.bg(bg);
    }
    s
}

/// The summary bar, `width` cells: `Selected 2.4 GiB of 3.1 GiB · 6 of 14 items` and what the
/// view adds (`extra`) on the left, the key that deletes on the right (left out in a read-only
/// browser). `noun` names what is counted, in the plural.
pub fn summary(app: &App, tally: Tally, noun: &str, extra: &str, width: u16) -> Line<'static> {
    let t = &app.theme;
    let selected = format!(
        " Selected {} of {} · {} of {} {noun}",
        human(tally.ticked_size),
        human(tally.size),
        tally.ticked,
        tally.items
    );
    let right = if app.read_only {
        String::new()
    } else {
        format!("{} delete selected ", first_key(Action::Trash))
    };
    let rw = Span::raw(&right).width();
    let room = usize::from(width).saturating_sub(rw + 1);
    // What the view adds gives way first.
    let left = format!("{selected}{extra}");
    let left = if Span::raw(&left).width() <= room {
        left
    } else {
        truncate_middle(&selected, room)
    };
    let pad = usize::from(width).saturating_sub(Span::raw(&left).width() + rw);
    Line::from(vec![
        Span::raw(left),
        Span::raw(" ".repeat(pad)),
        Span::styled(right, Style::new().add_modifier(Modifier::BOLD)),
    ])
    .style(bar_style(t))
}

/// The state of a row's checkbox.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Tick {
    On,
    Off,
    /// A section with some of its items ticked.
    Partial,
    /// The copy of a group of duplicates that is kept.
    Keep,
    /// Cannot be ticked (protected).
    Locked,
}

/// The checkbox, [`CHECK_WIDTH`] cells: `[x]`, `[ ]`, `[-]`, `KEEP` or `[⊘]`.
pub fn checkbox(t: &Theme, tick: Tick) -> Span<'static> {
    match tick {
        Tick::On => Span::styled("[x] ", t.mark),
        Tick::Off => Span::styled("[ ] ", t.fg_dim),
        Tick::Partial => Span::styled("[-] ", t.mark),
        Tick::Keep => Span::styled("KEEP", t.status_ok.add_modifier(Modifier::BOLD)),
        Tick::Locked => Span::styled("[⊘] ", t.fg_dim),
    }
}

/// The tick of a section whose items have `ticked` of `of` ticked.
pub fn tick_of(ticked: usize, of: usize) -> Tick {
    match ticked {
        0 => Tick::Off,
        n if n >= of => Tick::On,
        _ => Tick::Partial,
    }
}

/// How long ago `mtime` was at `now` (both seconds since the epoch), in a few cells: `5 h`,
/// `3 d`, `5 mo`, `2 y`; `—` when unknown or before 1990.
pub fn age(mtime: i64, now: i64) -> String {
    if mtime < KNOWN_SINCE {
        return "—".into();
    }
    let secs = now.saturating_sub(mtime).max(0);
    let days = secs / 86_400;
    match days {
        0 => format!("{} h", secs / 3_600),
        1..60 => format!("{days} d"),
        60..365 => format!("{} mo", days / 30),
        _ => format!("{} y", days / 365),
    }
}

/// The number of leading characters all of `paths` share, up to and including their last
/// shared `/`; 0 for fewer than two paths.
pub fn shared_prefix<'a>(paths: impl IntoIterator<Item = &'a str>) -> usize {
    let mut it = paths.into_iter();
    let Some(first) = it.next() else {
        return 0;
    };
    let mut shared: Vec<char> = first.chars().collect();
    let mut n = 0;
    for p in it {
        n += 1;
        let same = shared.iter().zip(p.chars()).take_while(|(a, b)| **a == *b);
        let k = same.count();
        shared.truncate(k);
    }
    if n == 0 {
        return 0;
    }
    shared
        .iter()
        .rposition(|&c| c == std::path::MAIN_SEPARATOR || c == '/')
        .map_or(0, |i| i + 1)
}

/// `p` in at most `width` cells, its first `dim` characters (the part all rows share) dimmed.
/// Too long, it loses the middle of its directories and keeps its last two components whole
/// when they fit: `photos/2026/trip…/edited/final.jpg`.
pub fn path(t: &Theme, p: &str, dim: usize, width: usize, style: Style) -> Vec<Span<'static>> {
    let shown = shorten(p, width);
    // The shared part stays dimmed up to where the middle was cut out.
    let cut = shown
        .find('…')
        .map_or(usize::MAX, |b| shown[..b].chars().count());
    let dim = dim.min(cut).min(shown.chars().count());
    let split = shown
        .char_indices()
        .nth(dim)
        .map_or(shown.len(), |(b, _)| b);
    let (head, tail) = shown.split_at(split);
    let mut spans = Vec::new();
    if !head.is_empty() {
        spans.push(Span::styled(head.to_owned(), t.fg_dim));
    }
    spans.push(Span::styled(tail.to_owned(), style));
    spans
}

/// `p` shortened to `width` cells, keeping its last two components (see [`path`]).
fn shorten(p: &str, width: usize) -> String {
    if Span::raw(p).width() <= width {
        return p.to_owned();
    }
    let seps: Vec<usize> = p
        .char_indices()
        .filter(|&(_, c)| c == '/' || c == std::path::MAIN_SEPARATOR)
        .map(|(i, _)| i)
        .collect();
    if seps.len() >= 2 {
        // The last two components with the separator before them.
        let at = seps[seps.len() - 2];
        let (head, tail) = p.split_at(at);
        let room = width.saturating_sub(Span::raw(tail).width());
        if room >= 2 {
            return format!("{}{tail}", truncate_middle(head, room));
        }
    }
    truncate_middle(p, width)
}

/// Where the rows and the detail pane go in `area`: the pane on the right from
/// [`DETAIL_RIGHT_FROM`] columns, else below the rows with at most `wants` lines. `None` for the
/// pane when there is no room for it.
pub fn split(area: Rect, wants: u16) -> (Rect, Option<Rect>) {
    if area.width >= DETAIL_RIGHT_FROM {
        let w = (area.width * DETAIL_SHARE / 100).clamp(DETAIL_MIN_WIDTH, DETAIL_MAX_WIDTH);
        let rows = Rect {
            width: area.width - w,
            ..area
        };
        let pane = Rect {
            x: area.x + area.width - w,
            width: w,
            ..area
        };
        return (rows, Some(pane));
    }
    let h = (wants + 1)
        .min(DETAIL_MAX_HEIGHT + 1)
        .min(area.height.saturating_sub(ROWS_MIN));
    // The rule above it, and two lines at least.
    if h < 3 {
        return (area, None);
    }
    let rows = Rect {
        height: area.height - h,
        ..area
    };
    let pane = Rect {
        y: area.y + area.height - h,
        height: h,
        ..area
    };
    (rows, Some(pane))
}

/// Draws `lines` in the detail pane `area` from [`split`], wrapped and a cell in from each
/// side: a rule on its left side when it is on the right of the rows, above it when it is
/// below them.
pub fn draw_detail(f: &mut Frame, t: &Theme, area: Rect, right: bool, lines: Vec<Line<'_>>) {
    let block = Block::new()
        .borders(if right { Borders::LEFT } else { Borders::TOP })
        .border_style(t.border)
        .padding(Padding::horizontal(1));
    f.render_widget(
        Paragraph::new(lines)
            .block(block)
            .wrap(Wrap { trim: false }),
        area,
    );
}

/// Whether the detail pane in `area` from [`split`] is on the right of the rows.
pub fn on_right(area: Rect) -> bool {
    area.width >= DETAIL_RIGHT_FROM
}

/// `d` and `D` with nothing ticked: says how to tick, so that the delete keys never act on the
/// list's selection hidden behind the view. `None` passes the key on.
pub fn delete_needs_ticks(app: &mut App, k: &KeyEvent) -> Option<Vec<Effect>> {
    match keys::action_for(k)? {
        Action::Trash | Action::DeletePermanent if app.marks.is_empty() && !app.read_only => {
            app.status = Some(format!(
                "tick items with {} first: {} and {} delete the ticked ones",
                first_key(Action::ToggleMark),
                first_key(Action::Trash),
                first_key(Action::DeletePermanent)
            ));
            Some(Vec::new())
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: i64 = 1_792_324_800;

    #[test]
    fn ages_are_relative_and_unknown_times_are_a_dash() {
        assert_eq!(age(NOW - 5 * 3_600, NOW), "5 h");
        assert_eq!(age(NOW - 3 * 86_400, NOW), "3 d");
        assert_eq!(age(NOW - 150 * 86_400, NOW), "5 mo");
        assert_eq!(age(NOW - 800 * 86_400, NOW), "2 y");
        assert_eq!(age(0, NOW), "—");
        assert_eq!(age(KNOWN_SINCE - 1, NOW), "—");
        // A time in the future is now.
        assert_eq!(age(NOW + 60, NOW), "0 h");
    }

    #[test]
    fn paths_keep_their_last_two_components() {
        let p = "photos/2026/trip-to-the-coast/edited/final.jpg";
        assert_eq!(shorten(p, 100), p);
        let short = shorten(p, 30);
        assert!(short.ends_with("/edited/final.jpg"), "{short}");
        assert!(
            short.contains('…') && Span::raw(&short).width() <= 30,
            "{short}"
        );
        // Without room for them, the middle of the whole path goes.
        let tiny = shorten(p, 10);
        assert!(
            tiny.contains('…') && Span::raw(&tiny).width() <= 10,
            "{tiny}"
        );
    }

    #[test]
    fn the_shared_prefix_ends_at_a_separator() {
        let paths = ["~/dev/a/x", "~/dev/ab/y"];
        assert_eq!(shared_prefix(paths), "~/dev/".len());
        assert_eq!(shared_prefix(["~/dev/a"]), 0);
        assert_eq!(shared_prefix(["a/x", "b/y"]), 0);
        let t = Theme::no_color();
        let spans = path(&t, "~/dev/a/x", 6, 40, Style::new());
        assert_eq!(spans[0].content, "~/dev/");
        assert_eq!(spans[0].style, t.fg_dim);
        assert_eq!(spans[1].content, "a/x");
    }

    #[test]
    fn sections_tick_partly() {
        assert_eq!(tick_of(0, 3), Tick::Off);
        assert_eq!(tick_of(2, 3), Tick::Partial);
        assert_eq!(tick_of(3, 3), Tick::On);
        let t = Theme::no_color();
        for tick in [Tick::On, Tick::Off, Tick::Partial, Tick::Keep, Tick::Locked] {
            assert_eq!(checkbox(&t, tick).width(), CHECK_WIDTH, "{tick:?}");
        }
    }

    #[test]
    fn the_detail_pane_goes_right_when_wide_and_below_when_narrow() {
        let (rows, pane) = split(Rect::new(0, 0, 120, 20), 4);
        let pane = pane.expect("a pane");
        assert_eq!(rows.width + pane.width, 120);
        assert_eq!(pane.height, 20);
        let (rows, pane) = split(Rect::new(0, 0, 80, 20), 4);
        let pane = pane.expect("a pane");
        assert_eq!((rows.height, pane.height), (15, 5));
        // Too short for both: the rows only.
        assert!(split(Rect::new(0, 0, 80, 10), 4).1.is_none());
    }
}
