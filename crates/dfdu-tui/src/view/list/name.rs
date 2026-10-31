//! The name cell of a list row: the name, shortened in the middle when it does not fit, the
//! rank arrow, and the notes after it in a fixed order, cut to the room left.

use ratatui::style::Style;
use ratatui::text::Span;

use crate::app::App;
use crate::format::{count, cut_end, truncate_middle};
use crate::model::outside;
use crate::model::snapshot::Mark;
use crate::state::{filter, motion};
use crate::theme::Theme;

/// A note is cut only when at least this many cells of it can show, with the `…`.
const NOTE_MIN: usize = 8;

/// The name cell of entry `index`, `width` cells wide: the name and rank arrow, then the notes
/// that fit. The notes come apart so a ghost's are not struck through with its name.
pub fn cell(app: &App, index: usize, width: usize) -> (Vec<Span<'static>>, Vec<Span<'static>>) {
    let t = &app.theme;
    let e = &app.snap.entries[index];
    let name = String::from_utf8_lossy(&e.name).into_owned();
    let style = if e.is_dir { t.dir } else { Style::new() };
    let mut spans = filter::name_spans(app, name, style);
    if e.is_dir {
        spans.push(Span::styled("/", style));
    }
    let mut spans = cut_middle(spans, width, style);
    let mut left = width.saturating_sub(spans.iter().map(Span::width).sum());
    if let Some(r) = motion::rank(app, e).filter(|r| r.width() <= left) {
        left -= r.width();
        spans.push(r);
    }
    let mut after = Vec::new();
    for (text, style) in notes(app, index, t) {
        let w = Span::raw(&text).width();
        if w <= left {
            left -= w;
            after.push(Span::styled(text, style));
            continue;
        }
        if left >= NOTE_MIN {
            after.push(Span::styled(cut_end(&text, left), style));
        }
        break;
    }
    (spans, after)
}

/// The notes after `e`'s name, each starting with two spaces, most important first: that it is
/// deleted, what went wrong reading it or inside it, its mark, its subvolume label, and that its
/// size is only an upper bound.
fn notes(app: &App, index: usize, t: &Theme) -> Vec<(String, Style)> {
    let e = &app.snap.entries[index];
    let mut notes = Vec::new();
    if e.ghost {
        notes.push(("  deleted".to_owned(), t.fg_dim));
    }
    if let Some(p) = &e.problem {
        notes.push((format!("  {p}"), t.status_error));
    } else if e.mark == Mark::Error {
        let what = if e.is_dir {
            "  incomplete"
        } else {
            "  could not be read"
        };
        notes.push((what.to_owned(), t.status_error));
    }
    match e.errors_inside {
        0 => {}
        1 => notes.push(("  1 error inside".to_owned(), t.status_error)),
        n => notes.push((format!("  {} errors inside", count(n)), t.status_error)),
    }
    notes.extend(mark_note(app, index, t));
    if let Some(l) = &e.label {
        notes.push((format!("  [{l}]"), t.fg_dim));
    }
    if e.compressed {
        notes.push(("  compressed: at most this".to_owned(), t.fg_dim));
    }
    notes
}

/// What the row says about its mark after the name, if anything.
fn mark_note(app: &App, index: usize, t: &Theme) -> Option<(String, Style)> {
    let e = &app.snap.entries[index];
    let note = |s: &str, style| Some((s.to_owned(), style));
    match e.mark {
        Mark::Stalled => note("  not responding", t.status_warn),
        Mark::Rescanning => note("  rescanning", t.fg_dim),
        // Its type and used space; what it uses is not part of the sizes here.
        Mark::OtherFs | Mark::Network => Some((outside::row_note(app, index), t.fg_dim)),
        Mark::DupMount => note("  mounted again: counted at its other mount", t.fg_dim),
        Mark::Kernel => note("  kernel filesystem", t.fg_dim),
        Mark::Incomplete => note("  incomplete: r to rescan", t.status_warn),
        Mark::Deleting => note("  deleting", t.status_warn),
        _ => None,
    }
}

/// `spans` shortened to at most `width` cells by cutting out their middle
/// ([`truncate_middle`]), each kept character in its span's style and the `…` in `style`.
fn cut_middle(spans: Vec<Span<'static>>, width: usize, style: Style) -> Vec<Span<'static>> {
    let text: String = spans.iter().map(|s| s.content.as_ref()).collect();
    let cut = truncate_middle(&text, width);
    if cut == text {
        return spans;
    }
    // The cut is a start of `text`, the `…` and an end of it.
    let (total, kept) = (text.chars().count(), cut.chars().count());
    let tail = text
        .chars()
        .rev()
        .zip(cut.chars().rev())
        .take_while(|(a, b)| a == b)
        .count()
        .min(kept.saturating_sub(1));
    let head = kept.saturating_sub(1 + tail);
    let (mut start, mut end) = (Vec::new(), Vec::new());
    let mut i = 0;
    for s in &spans {
        let (mut h, mut t) = (String::new(), String::new());
        for c in s.content.chars() {
            if i < head {
                h.push(c);
            } else if i >= total - tail {
                t.push(c);
            }
            i += 1;
        }
        if !h.is_empty() {
            start.push(Span::styled(h, s.style));
        }
        if !t.is_empty() {
            end.push(Span::styled(t, s.style));
        }
    }
    if kept > 0 {
        start.push(Span::styled("…", style));
    }
    start.extend(end);
    start
}

#[cfg(test)]
mod tests {
    use ratatui::style::{Color, Style};

    use super::*;

    #[test]
    fn names_are_cut_in_the_middle_keeping_their_styles() {
        let hit = Style::new().fg(Color::Yellow);
        let spans = vec![
            Span::raw("screen-"),
            Span::styled("rec", hit),
            Span::raw("ording-2026-10-18-walkthrough-final.mov"),
        ];
        let cut = cut_middle(spans.clone(), 20, Style::new());
        let text: String = cut.iter().map(|s| s.content.as_ref()).collect();
        assert_eq!(
            text,
            truncate_middle("screen-recording-2026-10-18-walkthrough-final.mov", 20)
        );
        assert_eq!(cut[1], Span::styled("rec", hit));
        assert_eq!(cut_middle(spans.clone(), 80, Style::new()), spans);
    }
}
