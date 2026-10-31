//! The text of a review row: ages relative to now, and paths shortened in the middle with the
//! part every row shares dimmed.

use ratatui::style::Style;
use ratatui::text::Span;

use crate::format::truncate_middle;
use crate::theme::Theme;

/// Before this (1990-01-01) a modification time is taken as unknown: an archive's zero, or a
/// clock that was never set.
const KNOWN_SINCE: i64 = 631_152_000;

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
}
