//! Drawing. Works only from [`App`] and its snapshot, never from the engine model, so it runs
//! after the read lock is released.

use std::time::Instant;

use dut_engine::Mode;
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Clear, Paragraph};

use crate::app::{App, Overlay};
use crate::order::SortKey;
use crate::snapshot::{EntryData, Mark};
use crate::theme::{bar, count, human, spinner};

fn mmss(d: std::time::Duration) -> String {
    let s = d.as_secs();
    format!("{:02}:{:02}", s / 60, s % 60)
}

fn lossy(b: &[u8]) -> String {
    String::from_utf8_lossy(b).into_owned()
}

/// Draws the whole screen and updates the list height and scroll offset in `app`.
pub fn draw(f: &mut Frame, app: &mut App, now: Instant) {
    let area = f.area();
    let stalled_line = if app.snap.stalled > 0 { 1 } else { 0 };
    let [header, list, prompt, status, footer] = Layout::vertical([
        Constraint::Length(2),
        Constraint::Min(1),
        Constraint::Length(stalled_line),
        Constraint::Length(1),
        Constraint::Length(1),
    ])
    .areas(area);
    let frame = now.duration_since(app.started).as_millis() as u64 / 100;

    draw_header(f, app, header, now, frame);
    draw_list(f, app, list, frame);
    if stalled_line > 0 {
        let n = app.snap.stalled;
        let text = format!(
            " {n} director{} not responding: wait, or press x to skip {}",
            if n == 1 { "y is" } else { "ies are" },
            if n == 1 { "it" } else { "them" }
        );
        f.render_widget(Paragraph::new(Line::styled(text, app.theme.warn)), prompt);
    }
    let msg = match app.snap.deleting {
        Some((done, total)) => format!("deleting {done}/{total}…"),
        None => app
            .status
            .clone()
            .or_else(|| app.snap.notice.clone())
            .unwrap_or_default(),
    };
    f.render_widget(
        Paragraph::new(Line::styled(format!(" {msg}"), app.theme.dim)),
        status,
    );
    let hints = "↑↓ move  → open  ← up  s n C M sort  a apparent  r rescan  d trash  D delete  ? help  q quit";
    f.render_widget(
        Paragraph::new(Line::styled(format!(" {hints}"), app.theme.dim)),
        footer,
    );

    match app.overlay.clone() {
        Overlay::None => {}
        Overlay::Help => draw_help(f, app, area),
        Overlay::ConfirmDelete {
            name,
            size,
            is_dir,
            mode,
            typed,
            scanning,
            ..
        } => draw_confirm(f, app, area, &name, size, is_dir, mode, &typed, scanning),
    }
}

fn draw_header(f: &mut Frame, app: &App, area: Rect, now: Instant, frame: u64) {
    let t = &app.theme;
    let s = &app.snap;
    let state = match app.done_at {
        Some(done) if !s.busy => format!("done in {}", mmss(done.duration_since(app.started))),
        _ => format!(
            "{} scanning {}",
            spinner(frame),
            mmss(now.duration_since(app.started))
        ),
    };
    let path = lossy(&s.dir_path);
    let left = vec![
        Span::styled(" dut ", t.title),
        Span::raw(" "),
        Span::styled(path, t.title),
    ];
    let right = vec![Span::raw(state), Span::raw(" ")];
    let line1 = spread(left, right, area.width);

    let root = s.root_total;
    let size = if app.apparent {
        root.apparent
    } else {
        root.alloc
    };
    let mut info = vec![Span::raw(format!(
        " {} items · {}",
        count(root.items),
        human(size)
    ))];
    if root.errors > 0 {
        info.push(Span::raw(" · "));
        info.push(Span::styled(
            format!("{} errors", count(root.errors)),
            t.error,
        ));
    }
    if s.incomplete {
        info.push(Span::styled(" · incomplete", t.warn));
    }
    let key = match app.sort.key {
        SortKey::Size => "size",
        SortKey::Name => "name",
        SortKey::Items => "items",
        SortKey::Mtime => "mtime",
    };
    let arrow = if app.sort.desc { "↓" } else { "↑" };
    let mode = if app.apparent {
        "apparent size"
    } else {
        "disk usage"
    };
    let right2 = vec![Span::styled(format!("sort: {key} {arrow}  {mode} "), t.dim)];
    let line2 = spread(info, right2, area.width);
    f.render_widget(Paragraph::new(vec![line1, line2]), area);
}

/// A line with `left` at the start and `right` at the end of `width` cells.
fn spread<'a>(left: Vec<Span<'a>>, right: Vec<Span<'a>>, width: u16) -> Line<'a> {
    let lw: usize = left.iter().map(|s| s.width()).sum();
    let rw: usize = right.iter().map(|s| s.width()).sum();
    let pad = (width as usize).saturating_sub(lw + rw);
    let mut spans = left;
    spans.push(Span::raw(" ".repeat(pad)));
    spans.extend(right);
    Line::from(spans)
}

fn bar_width(width: u16) -> usize {
    match width {
        100.. => 24,
        70..100 => 16,
        50..70 => 8,
        _ => 0,
    }
}

fn draw_list(f: &mut Frame, app: &mut App, area: Rect, frame: u64) {
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

    let t = app.theme.clone();
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
    let max = app
        .rows
        .iter()
        .map(|&i| app.snap.entries[i].size)
        .max()
        .unwrap_or(0)
        .max(1);
    let total = if app.apparent {
        app.snap.dir_total.apparent
    } else {
        app.snap.dir_total.alloc
    }
    .max(1);
    let bw = bar_width(area.width);
    let sel = app.selected_index();
    for (row, &i) in app
        .rows
        .iter()
        .enumerate()
        .skip(app.scroll)
        .take(height.saturating_sub(extra))
    {
        let e = &app.snap.entries[i];
        let mut line = entry_line(e, &t, frame, max, total, bw);
        if sel == Some(row) {
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

fn entry_line<'a>(
    e: &EntryData,
    t: &crate::theme::Theme,
    frame: u64,
    max: u64,
    total: u64,
    bw: usize,
) -> Line<'a> {
    let (glyph, gstyle) = match e.mark {
        Mark::Scanning | Mark::Rescanning => (spinner(frame + (e.dev_ino.1 % 10)), t.dim),
        Mark::Stalled => ('~', t.warn),
        Mark::Error => ('!', t.error),
        Mark::OtherFs => ('>', t.dim),
        Mark::Deleting => ('x', t.warn),
        Mark::Incomplete => ('?', t.warn),
        Mark::None => (' ', Style::new()),
    };
    let mut spans = vec![
        Span::styled(format!(" {glyph}"), gstyle),
        Span::raw(format!("{:>10}", human(e.size))),
    ];
    if bw > 0 {
        spans.push(Span::raw("  "));
        spans.push(Span::styled(bar(e.size as f64 / max as f64, bw), t.bar));
    }
    spans.push(Span::raw(format!(
        "  {:>5.1}%",
        e.size as f64 * 100.0 / total as f64
    )));
    spans.push(Span::styled(format!("  {:>6}", count(e.items)), t.dim));
    let name = String::from_utf8_lossy(&e.name).into_owned();
    if e.is_dir {
        spans.push(Span::styled(format!("  {name}/"), t.dir));
    } else {
        spans.push(Span::raw(format!("  {name}")));
    }
    let note = match e.mark {
        Mark::Stalled => Some(("  not responding", t.warn)),
        Mark::Rescanning => Some(("  rescanning", t.dim)),
        Mark::OtherFs => Some(("  other filesystem", t.dim)),
        Mark::Error if e.is_dir => Some(("  incomplete", t.error)),
        Mark::Incomplete => Some(("  incomplete: r to rescan", t.warn)),
        Mark::Deleting => Some(("  deleting", t.warn)),
        Mark::Error => Some(("  unreadable", t.error)),
        _ => None,
    };
    if let Some((s, st)) = note {
        spans.push(Span::styled(s, st));
    }
    Line::from(spans)
}

fn centered(area: Rect, w: u16, h: u16) -> Rect {
    let w = w.min(area.width);
    let h = h.min(area.height);
    Rect {
        x: area.x + (area.width - w) / 2,
        y: area.y + (area.height - h) / 2,
        width: w,
        height: h,
    }
}

fn draw_help(f: &mut Frame, app: &App, area: Rect) {
    let keys = [
        ("↑ ↓  k j", "move"),
        ("→ l Enter", "open directory"),
        ("← h Backspace", "parent directory"),
        ("PgUp PgDn g G", "page, first, last"),
        (
            "s n C M",
            "sort by size, name, items, mtime (again: reverse)",
        ),
        ("S", "re-sort now"),
        ("a", "toggle apparent size / disk usage"),
        ("e", "hide or show hidden files"),
        ("r", "rescan the selected directory"),
        ("x", "skip directories that are not responding"),
        ("d", "move to Trash"),
        ("D", "delete permanently (type delete)"),
        ("q Ctrl-C", "quit"),
    ];
    let mut lines: Vec<Line> = keys
        .iter()
        .map(|(k, d)| {
            Line::from(vec![
                Span::styled(format!(" {k:<15}"), app.theme.key),
                Span::raw(*d),
            ])
        })
        .collect();
    lines.push(Line::raw(""));
    lines.push(Line::styled(
        " Rows hold their order while you navigate; they re-sort",
        app.theme.dim,
    ));
    lines.push(Line::styled(
        " 1.5 s after your last key, or when you press S.",
        app.theme.dim,
    ));
    let r = centered(area, 72, lines.len() as u16 + 2);
    f.render_widget(Clear, r);
    f.render_widget(
        Paragraph::new(lines).block(Block::bordered().title(" Keys ")),
        r,
    );
}

#[allow(clippy::too_many_arguments)]
fn draw_confirm(
    f: &mut Frame,
    app: &App,
    area: Rect,
    name: &str,
    size: u64,
    is_dir: bool,
    mode: Mode,
    typed: &str,
    scanning: bool,
) {
    let what = if is_dir {
        format!("{name}/")
    } else {
        name.to_string()
    };
    let size_line = if scanning {
        format!(" at least {} (still scanning)", human(size))
    } else {
        format!(" {}", human(size))
    };
    let mut lines = match mode {
        Mode::Trash => vec![Line::raw(format!(" Move {what} to the Trash?"))],
        Mode::Permanent => vec![
            Line::styled(format!(" Permanently delete {what}?"), app.theme.error),
            Line::raw(" This cannot be undone."),
        ],
    };
    lines.push(Line::styled(size_line, app.theme.dim));
    lines.push(Line::raw(""));
    lines.push(match mode {
        Mode::Trash => Line::from(vec![
            Span::styled(" y", app.theme.key),
            Span::raw(" confirm   "),
            Span::styled("n Esc", app.theme.key),
            Span::raw(" cancel"),
        ]),
        Mode::Permanent => Line::from(vec![
            Span::raw(" type "),
            Span::styled("delete", app.theme.key),
            Span::raw(" and Enter: "),
            Span::raw(typed.to_string()),
            Span::raw("   "),
            Span::styled("Esc", app.theme.key),
            Span::raw(" cancel"),
        ]),
    });
    let r = centered(area, 60, lines.len() as u16 + 2);
    f.render_widget(Clear, r);
    f.render_widget(
        Paragraph::new(lines).block(Block::bordered().title(" Delete ")),
        r,
    );
}
