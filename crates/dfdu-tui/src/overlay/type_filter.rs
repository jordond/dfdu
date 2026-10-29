//! The file type filter (`t`): shows only files of chosen types (video, audio, image...) or
//! extensions, everywhere below the current directory.
//!
//! While the filter is on, the snapshot counts only the matching files: a directory's size and
//! items are those of the matching files inside it, and entries without any are left out
//! ([`crate::model::snapshot::build_view`]). The picker lists the current directory's files by type and
//! by extension, read with the snapshot while it is open; toggling a row applies at once.
//! Directories cannot be marked or deleted while the filter is on, since the size shown is not
//! what deleting one would remove.

use std::cell::Cell;

use dfdu_tree::Totals;
use dfdu_tree::filetype::{Ext, FileType, TypeFilter};
use ratatui::Frame;
use ratatui::crossterm::event::{KeyCode, KeyEvent};
use ratatui::layout::{Constraint, Rect};
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Clear, Paragraph};

use crate::app::{App, Effect, Overlay};
use crate::input::line::{Edit, LineInput};
use crate::theme::human;

/// Why a directory cannot be marked or deleted while the filter is on.
pub const NO_FOLDERS: &str = "folders cannot be marked or deleted while the type filter is on: their sizes count only some files (Esc shows all)";

/// Extensions listed at most, largest first; chosen ones are always listed.
const MAX_EXTS: usize = 300;

/// State of the open picker, held in `Overlay::Types`.
#[derive(Clone, Debug, Default)]
pub struct TypePicker {
    input: LineInput,
    /// Index into [`rows`].
    cursor: usize,
    /// First row on screen, kept by drawing.
    scroll: Cell<usize>,
    /// Some row was toggled since the picker opened: `Enter` then only closes it.
    toggled: bool,
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
enum Row {
    /// Every file: choosing it clears the filter.
    All,
    Type(FileType),
    Ext(Ext),
}

/// Whether the type filter is on.
pub fn active(app: &App) -> bool {
    !app.types.is_empty()
}

/// Opens the picker.
pub fn open(app: &mut App) {
    app.overlay = Overlay::Types(TypePicker::default());
}

/// Whether the snapshot was read with another filter, or lacks the breakdown the open picker
/// shows.
pub fn stale(app: &App) -> bool {
    app.snap.types != app.types
        || (matches!(app.overlay, Overlay::Types(_)) && app.snap.breakdown.is_none())
}

/// Turns the filter off; false when it was off.
pub fn clear(app: &mut App) -> bool {
    if app.types.is_empty() {
        return false;
    }
    app.types = TypeFilter::default();
    app.status = Some("showing all files".into());
    true
}

/// The rows for `query`: every file, then the types and the extensions below the directory,
/// largest first, with their totals. Chosen types and extensions are listed even when the
/// directory has none, and a query that is an extension the directory lacks is offered too.
fn rows(app: &App, query: &str) -> Vec<(Row, Totals)> {
    let q = query.trim().to_ascii_lowercase();
    let size = |t: &Totals| app.size_mode.of(t);
    let mut out = Vec::new();
    let all = app
        .snap
        .breakdown
        .as_ref()
        .map(|b| {
            let mut t = Totals::default();
            for x in &b.by_type {
                t.add(x);
            }
            t
        })
        .unwrap_or_default();
    if q.is_empty() || "all files".contains(&q) {
        out.push((Row::All, all));
    }
    let wanted = |label: &str| q.is_empty() || label.contains(&q);
    let mut types: Vec<(Row, Totals)> = FileType::ALL
        .iter()
        .map(|&t| {
            let tot = app.snap.breakdown.as_ref().map(|b| b.by_type[t as usize]);
            (Row::Type(t), tot.unwrap_or_default())
        })
        .filter(|(r, t)| {
            let Row::Type(ty) = r else { return false };
            (t.items > 0 || app.types.has_type(*ty)) && wanted(ty.name())
        })
        .collect();
    types.sort_by_key(|(_, t)| std::cmp::Reverse(size(t)));
    out.extend(types);
    let mut exts: Vec<(Row, Totals)> = app
        .snap
        .breakdown
        .as_ref()
        .map(|b| b.exts_by(size))
        .unwrap_or_default()
        .into_iter()
        .filter(|(e, _)| wanted(&format!(".{e}")))
        .take(MAX_EXTS)
        .map(|(e, t)| (Row::Ext(e), t))
        .collect();
    for e in app.types.exts() {
        if !exts.iter().any(|(r, _)| *r == Row::Ext(*e)) && wanted(&format!(".{e}")) {
            exts.push((Row::Ext(*e), Totals::default()));
        }
    }
    if let Some(e) = Ext::new(q.as_bytes())
        && !exts.iter().any(|(r, _)| *r == Row::Ext(e))
    {
        exts.push((Row::Ext(e), Totals::default()));
    }
    out.extend(exts);
    out
}

fn chosen(f: &TypeFilter, r: Row) -> bool {
    match r {
        Row::All => f.is_empty(),
        Row::Type(t) => f.has_type(t),
        Row::Ext(e) => f.has_ext(&e),
    }
}

fn toggle(app: &mut App, r: Row) {
    match r {
        Row::All => app.types = TypeFilter::default(),
        Row::Type(t) => app.types.toggle_type(t),
        Row::Ext(e) => app.types.toggle_ext(e),
    }
    app.status = Some(if app.types.is_empty() {
        "showing all files".into()
    } else {
        format!("showing only {}", app.types)
    });
}

/// Typing searches the rows; `↑`/`↓`/`PgUp`/`PgDn` move, `Space` toggles the row, `Enter`
/// toggles it (unless something was toggled already) and closes, `Esc` closes.
pub fn key(app: &mut App, k: KeyEvent) -> Vec<Effect> {
    let page = super::list_page(app);
    let Overlay::Types(p) = &app.overlay else {
        return Vec::new();
    };
    let rows = rows(app, p.input.text());
    let last = rows.len().saturating_sub(1);
    let current = rows.get(p.cursor.min(last)).map(|r| r.0);
    let Overlay::Types(p) = &mut app.overlay else {
        return Vec::new();
    };
    if let Some(cursor) = super::list_cursor(k.code, p.cursor, last, page) {
        p.cursor = cursor;
        return Vec::new();
    }
    if k.code == KeyCode::Char(' ') {
        p.toggled = true;
        if let Some(r) = current {
            toggle(app, r);
        }
        return Vec::new();
    }
    match p.input.key(&k) {
        Edit::Changed => {
            p.cursor = 0;
            p.scroll.set(0);
        }
        Edit::Submit => {
            let toggled = p.toggled;
            app.overlay = Overlay::None;
            if let (false, Some(r)) = (toggled, current) {
                toggle(app, r);
            }
        }
        Edit::Cancel => app.overlay = Overlay::None,
        Edit::Moved | Edit::Ignored => {}
    }
    Vec::new()
}

/// Draws the picker in `area` and returns the rectangle it covers.
pub fn draw(f: &mut Frame, app: &App, area: Rect) -> Rect {
    let Overlay::Types(p) = &app.overlay else {
        return Rect::default();
    };
    let t = &app.theme;
    let rows = rows(app, p.input.text());
    let width = super::width(area, 64);
    let height = super::list_height(area, rows.len());
    let rect = area.centered(Constraint::Length(width), Constraint::Length(height));
    let inner = usize::from(width.saturating_sub(2));
    let fit = (height.saturating_sub(3) as usize).max(1);
    let cursor = p.cursor.min(rows.len().saturating_sub(1));
    let top = p
        .scroll
        .get()
        .min(cursor)
        .max((cursor + 1).saturating_sub(fit));
    p.scroll.set(top);

    let mut prompt = vec![Span::styled(" type or extension:", t.key), Span::raw(" ")];
    prompt.extend(p.input.spans(Style::new(), t.key));
    let mut lines = vec![Line::from(prompt)];
    if app.snap.breakdown.is_none() {
        lines.push(Line::styled(" reading…", t.fg_dim));
    } else if rows.is_empty() {
        lines.push(Line::styled(" nothing matches", t.fg_dim));
    }
    for (i, (row, tot)) in rows.iter().enumerate().skip(top).take(fit) {
        let mark = if chosen(&app.types, *row) {
            "[x]"
        } else {
            "[ ]"
        };
        let (label, kind) = match row {
            Row::All => ("all files".to_string(), String::new()),
            Row::Type(ty) => (ty.name().to_string(), String::new()),
            Row::Ext(e) => (format!(".{e}"), e.file_type().name().to_string()),
        };
        let files = match tot.items {
            1 => "1 file".to_string(),
            n => format!("{n} files"),
        };
        let left = format!(" {mark} {label:<12} {kind:<10}");
        let right = format!("{:>10}  {files:>11} ", human(app.size_mode.of(tot)));
        let pad = inner.saturating_sub(left.chars().count() + right.chars().count());
        let mut line = Line::from(vec![
            Span::raw(left),
            Span::raw(" ".repeat(pad)),
            Span::styled(right, t.fg_dim),
        ]);
        if i == cursor {
            line = line.style(t.cursor);
        }
        lines.push(line);
    }
    let title = if app.types.is_empty() {
        " File types ".to_string()
    } else {
        format!(" File types: {} ", app.types)
    };
    f.render_widget(Clear, rect);
    f.render_widget(
        Paragraph::new(lines).block(Block::bordered().title(title)),
        rect,
    );
    rect
}

#[cfg(test)]
mod tests {
    use std::time::Instant;

    use dfdu_tree::Kind;
    use ratatui::crossterm::event::KeyCode;

    use super::*;
    use crate::model::snapshot::{self, Target};
    use crate::tests::{press, render};
    use crate::theme::Theme;

    /// `/r` with `films/a.mp4` (100), `films/notes.txt` (10), `music/c.flac` (50), `x.mkv`
    /// (30) and `README` (5).
    fn model() -> dfdu_engine::Model {
        let mut ino = 0;
        let mut meta = |kind, size| {
            ino += 1;
            crate::tests::meta(kind, size, ino)
        };
        let mut m = dfdu_engine::Model::default();
        let t = &mut m.tree;
        let root = t.new_root(b"/r", &meta(Kind::Dir, 0), true);
        let films = t.add_dir(root, b"films", &meta(Kind::Dir, 0));
        let music = t.add_dir(root, b"music", &meta(Kind::Dir, 0));
        for (dir, name, size) in [
            (films, "a.mp4", 100),
            (films, "notes.txt", 10),
            (music, "c.flac", 50),
            (root, "x.mkv", 30),
            (root, "README", 5),
        ] {
            let f = t.add_file(dir, name.as_bytes(), &meta(Kind::File, size), false, None);
            let own = t.own(f);
            t.propagate_add(dir, &own);
        }
        m
    }

    /// A read as the run loop makes it.
    fn read(app: &mut App, m: &dfdu_engine::Model, now: Instant) {
        let mut target = app.target.clone();
        let breakdown = matches!(app.overlay, Overlay::Types(_));
        let snap = snapshot::build_view(m, &mut target, app.size_mode, &app.types, breakdown);
        app.target = target;
        app.refresh(snap, now);
    }

    fn names(app: &App) -> Vec<String> {
        let mut v: Vec<String> = app
            .snap
            .entries
            .iter()
            .map(|e| format!("{:?} {} {}", e.name, e.size, e.items))
            .collect();
        v.sort();
        v
    }

    #[test]
    fn picking_a_type_shows_only_its_files() {
        let m = model();
        let t0 = Instant::now();
        let mut app = App::new(Theme::no_color(), t0);
        app.target = Target::root();
        read(&mut app, &m, t0);
        assert_eq!(app.snap.entries.len(), 4);
        assert!(!stale(&app));

        app.key(press(KeyCode::Char('t')), t0);
        assert!(stale(&app), "the picker needs the breakdown");
        read(&mut app, &m, t0);
        let out = render(&mut app, t0, 90, 20);
        assert!(out.contains("File types"), "{out}");
        assert!(out.contains("[x] all files"), "{out}");
        assert!(out.contains("[ ] video"), "{out}");
        assert!(out.contains(".mkv"), "{out}");
        for c in "vid".chars() {
            app.key(press(KeyCode::Char(c)), t0);
        }
        app.key(press(KeyCode::Enter), t0);
        assert!(matches!(app.overlay, Overlay::None));
        assert_eq!(app.types.to_string(), "video");
        assert!(stale(&app));
        read(&mut app, &m, t0);
        assert_eq!(names(&app), ["\"films\" 100 1", "\"x.mkv\" 30 1"]);
        assert_eq!(
            (app.snap.dir_total.alloc, app.snap.dir_total.items),
            (130, 2)
        );
        let out = render(&mut app, t0, 100, 10);
        assert!(out.contains("only video: 130 B in 2 files here"), "{out}");

        // Folders cannot be marked or deleted; files can.
        let films = app.rows.iter().position(|&i| app.snap.entries[i].is_dir);
        app.move_to(films.unwrap());
        app.key(press(KeyCode::Char(' ')), t0);
        assert!(app.marks.is_empty());
        assert_eq!(app.status.as_deref(), Some(NO_FOLDERS));
        app.key(press(KeyCode::Char('d')), t0);
        assert!(matches!(app.overlay, Overlay::None));

        // An extension added by typing it, then Esc clears everything.
        app.key(press(KeyCode::Char('t')), t0);
        read(&mut app, &m, t0);
        for c in ".flac".chars() {
            app.key(press(KeyCode::Char(c)), t0);
        }
        app.key(press(KeyCode::Char(' ')), t0);
        app.key(press(KeyCode::Esc), t0);
        assert_eq!(app.types.to_string(), "video, .flac");
        read(&mut app, &m, t0);
        assert_eq!(app.snap.entries.len(), 3);
        app.key(press(KeyCode::Esc), t0);
        assert!(app.types.is_empty());
        read(&mut app, &m, t0);
        assert_eq!(app.snap.entries.len(), 4);
    }
}
