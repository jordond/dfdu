//! The entry list: one row per entry with its size, bar, share, items, name and notes.

use std::time::Instant;

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use crate::app::App;
use crate::snapshot::{EntryData, Mark};
use crate::theme::{Theme, bar, count, human, percent, ratio, spinner};
use crate::{filter, growth, marks, motion};

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
    let extra = usize::from(app.new_count > 0);
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
        lines.push(Line::styled(msg, t.fg_dim));
    }
    let max = app.rows_largest.max(1);
    let total = app.size_mode.of(&app.snap.dir_total).max(1);
    let bw = bar_width(area.width);
    let sel = app.selected_index();
    for (row, &i) in visible.iter().enumerate() {
        let mut line = entry_line(app, i, frame, max, total, bw);
        if sel == Some(app.scroll + row) {
            line = line.style(t.cursor);
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
            t.fg_dim,
        ));
    }
    f.render_widget(Paragraph::new(lines), area);
}

/// The glyph after the gutter that shows `e`'s mark, in its style; the spinner turns with
/// `frame`, offset per entry.
fn mark_glyph(e: &EntryData, t: &Theme, frame: u64) -> (char, Style) {
    match e.mark {
        Mark::Scanning | Mark::Rescanning => (spinner(frame + (e.dev_ino.1 % 10)), t.fg_dim),
        Mark::Stalled => ('~', t.status_warn),
        Mark::Error => ('!', t.status_error),
        Mark::OtherFs | Mark::Network | Mark::Kernel => ('>', t.fg_dim),
        Mark::DupMount => ('=', t.fg_dim),
        Mark::ErrorsInside => ('.', t.status_error),
        Mark::Deleting => ('x', t.status_warn),
        Mark::Incomplete => ('?', t.status_warn),
        Mark::None => (' ', Style::new()),
    }
}

/// What the row says about `mark` after the name, if anything.
fn mark_note(mark: Mark, t: &Theme) -> Option<(&'static str, Style)> {
    match mark {
        Mark::Stalled => Some(("  not responding", t.status_warn)),
        Mark::Rescanning => Some(("  rescanning", t.fg_dim)),
        Mark::OtherFs => Some(("  other filesystem", t.fg_dim)),
        Mark::DupMount => Some(("  mounted again: counted at its other mount", t.fg_dim)),
        Mark::Network => Some((
            "  network filesystem: start with --network to scan",
            t.fg_dim,
        )),
        Mark::Kernel => Some(("  kernel filesystem", t.fg_dim)),
        Mark::Incomplete => Some(("  incomplete: r to rescan", t.status_warn)),
        Mark::Deleting => Some(("  deleting", t.status_warn)),
        _ => None,
    }
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
    let (glyph, gstyle) = mark_glyph(e, t, frame);
    // The gutter cell takes the mark's style where the gutter sets none of its own.
    let gutter = marks::gutter(app, e);
    // A size that can still grow, or is only a lower bound, is dim.
    let size_style = match e.mark {
        Mark::Scanning
        | Mark::Rescanning
        | Mark::Stalled
        | Mark::Incomplete
        | Mark::Error
        | Mark::ErrorsInside => t.fg_dim,
        _ => Style::new(),
    };
    let mut spans = vec![
        Span::styled(gutter.content, gstyle.patch(gutter.style)),
        Span::styled(glyph.to_string(), gstyle),
        Span::styled(format!("{:>10}", human(e.size)), size_style),
    ];
    if bw > 0 {
        let frac = motion::bar(app, e, ratio(e.size, max));
        spans.push(Span::raw("  "));
        spans.push(Span::styled(bar(frac, bw), t.bar_fill));
    }
    // A ghost's size is no longer part of the total: its share would be meaningless.
    spans.push(Span::raw(if e.ghost {
        " ".repeat(8)
    } else {
        format!("  {:>5.1}%", percent(e.size, total))
    }));
    spans.push(Span::styled(format!("  {:>6}", count(e.items)), t.fg_dim));
    if let Some(rules) = &app.snap.rules {
        // Inside a match, rows show its category dim: the match is the directory above.
        let (text, style) = match e.tag.map(|g| (g, g.category(rules))) {
            Some((g, (name, _))) if g.inherited => (name, t.fg_dim),
            Some((_, (name, slot))) => (name, t.cat[slot]),
            None => ("", Style::new()),
        };
        spans.push(Span::styled(format!("  {text:<10}"), style));
    }
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
    let note = mark_note(e.mark, t);
    if let Some(p) = &e.problem {
        spans.push(Span::styled(format!("  {p}"), t.status_error));
    } else if e.mark == Mark::Error {
        let what = if e.is_dir {
            "  incomplete"
        } else {
            "  could not be read"
        };
        spans.push(Span::styled(what, t.status_error));
    }
    match e.errors_inside {
        0 => {}
        1 => spans.push(Span::styled("  1 error inside", t.status_error)),
        n => spans.push(Span::styled(
            format!("  {} errors inside", count(n)),
            t.status_error,
        )),
    }
    if let Some(l) = &e.label {
        spans.push(Span::styled(format!("  [{l}]"), t.fg_dim));
    }
    if e.compressed {
        spans.push(Span::styled("  compressed: at most this", t.fg_dim));
    }
    if e.shared_out > 0 {
        spans.push(Span::styled(
            format!("  {} also used outside the scan", human(e.shared_out)),
            t.fg_dim,
        ));
    }
    if let Some((s, st)) = note {
        spans.push(Span::styled(s, st));
    }
    if app.growth.is_some() && !e.ghost && !e.pseudo {
        let path = app.snap.dir_path.join(dfdu_tree::name_os(&e.name));
        if let Some((s, grew)) = growth::row_note(app, &path, e.size, e.mark) {
            spans.push(Span::styled(
                format!("  {s}"),
                if grew { t.status_warn } else { t.fg_dim },
            ));
        }
    }
    if e.ghost {
        let struck = t.fg_dim.add_modifier(Modifier::CROSSED_OUT);
        for s in &mut spans {
            s.style = s.style.patch(struck);
        }
        spans.push(Span::styled("  deleted", t.fg_dim));
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
    use crate::tests::{press, snap_without_totals};
    use crate::theme::Theme;

    /// [`snap_without_totals`] of a scan that has finished.
    fn finished_snap(entries: Vec<EntryData>, version: u64) -> Snapshot {
        Snapshot {
            busy: false,
            ..snap_without_totals(entries, version)
        }
    }

    /// Rows note what grew or shrank noticeably since the last recorded scan; small changes,
    /// entries the history does not have and rows still scanning get no note.
    #[test]
    fn rows_note_growth_since_the_last_scan() {
        use dfdu_store::history::{Growth, Sizes};
        let t0 = Instant::now();
        let mut app = App::new(Theme::no_color(), t0);
        let mib = 1u64 << 20;
        let then = |disk: u64| Sizes {
            disk,
            apparent: disk,
            entries: 1,
        };
        app.growth = Some(std::sync::Arc::new(Growth {
            root: PathBuf::from("/r"),
            at: 86_400 * 3,
            sizes: [
                (b"grew".to_vec(), then(mib)),
                (b"shrank".to_vec(), then(9 * mib)),
                (b"same".to_vec(), then(4 * mib + 1000)),
                (b"busy".to_vec(), then(mib)),
            ]
            .into_iter()
            .collect(),
        }));
        let busy = EntryData {
            mark: Mark::Scanning,
            ..test_entry(4, "busy", 7 * mib)
        };
        app.refresh(
            finished_snap(
                vec![
                    test_entry(0, "grew", 5 * mib),
                    test_entry(1, "shrank", 2 * mib),
                    test_entry(2, "same", 4 * mib),
                    test_entry(3, "unknown", 3 * mib),
                    busy,
                ],
                1,
            ),
            t0,
        );
        let mut term = Terminal::new(TestBackend::new(120, 10)).expect("terminal");
        term.draw(|f| crate::view::draw(f, &mut app, t0))
            .expect("draw");
        let buf = term.backend().buffer();
        let rows: Vec<String> = (0..10)
            .map(|y| (0..120).map(|x| buf[(x, y)].symbol()).collect())
            .collect();
        let row = |name: &str| {
            rows.iter()
                .find(|r| r.contains(&format!("  {name}")))
                .unwrap_or_else(|| panic!("{name}: {rows:#?}"))
        };
        assert!(
            row("grew").contains("+4.0 MiB since 1970-01-04"),
            "{rows:#?}"
        );
        assert!(
            row("shrank").contains("-7.0 MiB since 1970-01-04"),
            "{rows:#?}"
        );
        for quiet in ["same", "unknown", "busy"] {
            assert!(!row(quiet).contains("since"), "{quiet}: {rows:#?}");
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
        app.refresh(
            finished_snap(vec![dir.clone(), test_entry(1, "kept", 10)], 1),
            t0,
        );
        // The deleted directory stays in its row as a ghost until the next re-sort.
        let ghost = EntryData {
            ghost: true,
            pseudo: true,
            is_dir: false,
            ..dir
        };
        app.refresh(finished_snap(vec![ghost, test_entry(1, "kept", 10)], 2), t0);
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
            buf[(u16::try_from(at).unwrap(), 2)]
                .modifier
                .contains(Modifier::CROSSED_OUT),
            "{row}"
        );

        for code in [KeyCode::Enter, KeyCode::Char('d'), KeyCode::Char('D')] {
            assert!(app.key(press(code), t0).is_empty());
            assert!(matches!(app.overlay, Overlay::None), "{code:?}");
            assert_eq!(app.target.path, PathBuf::new(), "{code:?} opened the ghost");
        }
        // A re-sort drops the ghost.
        app.key(press(KeyCode::Char('S')), t0);
        app.reorder(t0);
        assert_eq!(app.rows.len(), 1);
    }

    #[test]
    fn ghost_rows_have_no_share() {
        let t0 = Instant::now();
        let mut app = App::new(Theme::no_color(), t0);
        let (gone, kept) = (test_entry(0, "gone", 4 << 20), test_entry(1, "kept", 1));
        app.refresh(finished_snap(vec![gone.clone(), kept.clone()], 1), t0);
        // 4 MiB deleted beside 1 byte left: the total no longer counts the ghost.
        let ghost = EntryData {
            ghost: true,
            pseudo: true,
            ..gone
        };
        let mut s = finished_snap(vec![ghost, kept], 2);
        s.dir_total.alloc = 1;
        app.refresh(s, t0);
        assert_eq!(app.rows.len(), 2);
        let out = crate::tests::render(&mut app, t0, 80, 6);
        let row = |name: &str| {
            out.lines()
                .find(|l| l.contains(name))
                .expect("row")
                .to_string()
        };
        let (gone, kept) = (row("gone"), row("kept"));
        assert!(!gone.contains('%'), "{out}");
        assert!(kept.contains("100.0%"), "{out}");
        // The columns after the share stay in place.
        let column = |row: &str, name| row.split(name).next().map(|s| s.chars().count());
        assert_eq!(column(&gone, "gone"), column(&kept, "kept"), "{out}");
    }
}
