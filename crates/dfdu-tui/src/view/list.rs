//! The entry list: one row per entry with its size, bar, share, items, name and notes.

use std::time::Instant;

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use crate::app::App;
use crate::snapshot::Mark;
use crate::theme::{bar, count, human, spinner};
use crate::{filter, marks, motion};

fn bar_width(width: u16) -> usize {
    match width {
        100.. => 24,
        70..100 => 16,
        50..70 => 8,
        _ => 0,
    }
}

pub fn draw(f: &mut Frame, app: &mut App, area: Rect, now: Instant, frame: u64) {
    let height = area.height as usize;
    app.list_height = height;
    let extra = (app.new_count > 0) as usize;
    let rows = app.rows.len();
    // Never scroll further than needed to fill the screen, then keep the selection visible.
    app.scroll = app.scroll.min((rows + extra).saturating_sub(height));
    if let Some(sel) = app.selected_index() {
        if sel < app.scroll {
            app.scroll = sel;
        } else if sel >= app.scroll + height.saturating_sub(extra).max(1) {
            app.scroll = sel + 1 - height.saturating_sub(extra).max(1);
        }
    }
    app.scroll = app.scroll.min(rows.saturating_sub(1));
    // Entry indices of the rows on screen.
    let visible: Vec<usize> = app
        .rows
        .iter()
        .skip(app.scroll)
        .take(height.saturating_sub(extra))
        .copied()
        .collect();
    motion::frame(app, now, &visible);

    let app = &*app;
    let t = &app.theme;
    let mut lines = Vec::with_capacity(height);
    if rows == 0 && app.new_count == 0 {
        let msg = match app.snap.dir_mark {
            Some(Mark::Scanning | Mark::Rescanning | Mark::Stalled) => "  (scanning…)",
            Some(Mark::Error) => "  (could not be read)",
            Some(Mark::Incomplete) => "  (incomplete: press r to rescan)",
            _ => "  (empty)",
        };
        lines.push(Line::styled(msg, t.dim));
    }
    let max = app.rows_largest.max(1);
    let total = app.size_mode.of(&app.snap.dir_total).max(1);
    let bw = bar_width(area.width);
    let sel = app.selected_index();
    for (row, &i) in visible.iter().enumerate() {
        let mut line = entry_line(app, i, frame, max, total, bw);
        if sel == Some(app.scroll + row) {
            line = line.style(t.selected);
        }
        lines.push(line);
    }
    if app.new_count > 0 && lines.len() < height {
        let n = app.new_count;
        lines.push(Line::styled(
            format!(
                "   + {n} new entr{} since last sort (largest {})   S to sort now",
                if n == 1 { "y" } else { "ies" },
                human(app.new_largest)
            ),
            t.dim,
        ));
    }
    f.render_widget(Paragraph::new(lines), area);
}

/// The row of entry `index` of the snapshot. A ghost (a deleted entry kept in its place until
/// the next re-sort) is struck through and dimmed, and says so.
fn entry_line(
    app: &App,
    index: usize,
    frame: u64,
    max: u64,
    total: u64,
    bw: usize,
) -> Line<'static> {
    let t = &app.theme;
    let e = &app.snap.entries[index];
    let (glyph, gstyle) = match e.mark {
        Mark::Scanning | Mark::Rescanning => (spinner(frame + (e.dev_ino.1 % 10)), t.dim),
        Mark::Stalled => ('~', t.warn),
        Mark::Error => ('!', t.error),
        Mark::OtherFs => ('>', t.dim),
        Mark::DupMount => ('=', t.dim),
        Mark::Network | Mark::Kernel => ('>', t.dim),
        Mark::ErrorsInside => ('.', t.error),
        Mark::Deleting => ('x', t.warn),
        Mark::Incomplete => ('?', t.warn),
        Mark::None => (' ', Style::new()),
    };
    // The gutter cell takes the mark's style where the gutter sets none of its own.
    let gutter = marks::gutter(app, e);
    let mut spans = vec![
        Span::styled(gutter.content, gstyle.patch(gutter.style)),
        Span::styled(glyph.to_string(), gstyle),
        Span::raw(format!("{:>10}", human(e.size))),
    ];
    if bw > 0 {
        let frac = motion::bar(app, e, e.size as f64 / max as f64);
        spans.push(Span::raw("  "));
        spans.push(Span::styled(bar(frac, bw), t.bar));
    }
    spans.push(Span::raw(format!(
        "  {:>5.1}%",
        e.size as f64 * 100.0 / total as f64
    )));
    spans.push(Span::styled(format!("  {:>6}", count(e.items)), t.dim));
    let name = String::from_utf8_lossy(&e.name).into_owned();
    let name_style = if e.is_dir { t.dir } else { Style::new() };
    spans.push(Span::styled("  ", name_style));
    spans.extend(filter::name_spans(app, index, name, name_style));
    if e.is_dir {
        spans.push(Span::styled("/", name_style));
    }
    if let Some(r) = motion::rank(app, e) {
        spans.push(r);
    }
    let note = match e.mark {
        Mark::Stalled => Some(("  not responding", t.warn)),
        Mark::Rescanning => Some(("  rescanning", t.dim)),
        Mark::OtherFs => Some(("  other filesystem", t.dim)),
        Mark::DupMount => Some(("  mounted again: counted at its other mount", t.dim)),
        Mark::Network => Some(("  network filesystem: start with --network to scan", t.dim)),
        Mark::Kernel => Some(("  kernel filesystem", t.dim)),
        Mark::Incomplete => Some(("  incomplete: r to rescan", t.warn)),
        Mark::Deleting => Some(("  deleting", t.warn)),
        _ => None,
    };
    if let Some(p) = &e.problem {
        spans.push(Span::styled(format!("  {p}"), t.error));
    } else if e.mark == Mark::Error {
        let what = if e.is_dir {
            "  incomplete"
        } else {
            "  could not be read"
        };
        spans.push(Span::styled(what, t.error));
    }
    match e.errors_inside {
        0 => {}
        1 => spans.push(Span::styled("  1 error inside", t.error)),
        n => spans.push(Span::styled(
            format!("  {} errors inside", count(n)),
            t.error,
        )),
    }
    if let Some(l) = &e.label {
        spans.push(Span::styled(format!("  [{l}]"), t.dim));
    }
    if e.compressed {
        spans.push(Span::styled("  compressed: at most this", t.dim));
    }
    if e.shared_out > 0 {
        spans.push(Span::styled(
            format!("  {} also used outside the scan", human(e.shared_out)),
            t.dim,
        ));
    }
    if let Some((s, st)) = note {
        spans.push(Span::styled(s, st));
    }
    if e.ghost {
        let struck = t.dim.add_modifier(Modifier::CROSSED_OUT);
        for s in &mut spans {
            s.style = s.style.patch(struck);
        }
        spans.push(Span::styled("  deleted", t.dim));
    }
    Line::from(spans)
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use ratatui::crossterm::event::KeyCode;

    use super::*;
    use crate::app::Overlay;
    use crate::snapshot::{EntryData, Snapshot, test_entry};
    use crate::tests::key;
    use crate::theme::Theme;

    fn snap(entries: Vec<EntryData>, version: u64) -> Snapshot {
        Snapshot {
            dir_path: PathBuf::from("/r"),
            dir_node: Some(test_entry(99, "/r", 0).node),
            at_root: true,
            entries,
            version,
            ..Default::default()
        }
    }

    #[test]
    fn ghost_rows_are_struck_through_and_cannot_be_opened_or_deleted() {
        let t0 = Instant::now();
        let mut app = App::new(Theme::no_color(), t0);
        let dir = EntryData {
            is_dir: true,
            ..test_entry(0, "gone", 500)
        };
        app.refresh(snap(vec![dir.clone(), test_entry(1, "kept", 10)], 1), t0);
        // The deleted directory stays in its row as a ghost until the next re-sort.
        let ghost = EntryData {
            ghost: true,
            pseudo: true,
            is_dir: false,
            ..dir
        };
        app.refresh(snap(vec![ghost, test_entry(1, "kept", 10)], 2), t0);
        assert_eq!(app.rows.len(), 2);
        assert!(app.selected_entry().expect("selected").ghost);

        let mut term = Terminal::new(TestBackend::new(80, 6)).expect("terminal");
        term.draw(|f| crate::view::draw(f, &mut app, t0))
            .expect("draw");
        let buf = term.backend().buffer();
        let row: String = (0..80).map(|x| buf[(x, 2)].symbol()).collect();
        assert!(row.contains("gone  deleted"), "{row}");
        let cells: Vec<&str> = (0..80).map(|x| buf[(x, 2)].symbol()).collect();
        let at = (0..76)
            .find(|&x| cells[x..x + 4].concat() == "gone")
            .expect("name");
        assert!(
            buf[(at as u16, 2)].modifier.contains(Modifier::CROSSED_OUT),
            "{row}"
        );

        for code in [KeyCode::Enter, KeyCode::Char('d'), KeyCode::Char('D')] {
            assert!(app.key(key(code), t0).is_empty());
            assert!(matches!(app.overlay, Overlay::None), "{code:?}");
            assert_eq!(app.target.path, PathBuf::new(), "{code:?} opened the ghost");
        }
        // A re-sort drops the ghost.
        app.key(key(KeyCode::Char('S')), t0);
        app.reorder(t0);
        assert_eq!(app.rows.len(), 1);
    }
}
