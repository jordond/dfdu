//! The filter of the current directory (`/`): hides the rows whose names do not match.
//!
//! The query is matched fuzzily ([`Fuzzy`]) against each name as it is drawn (lossy UTF-8).
//! Whether each entry matches is kept per index of the snapshot's entries, so new entries of a
//! running scan are tested as they arrive, and a query that only grew re-tests only the entries
//! that matched before. The filter only hides rows: the rest keep their sort order.

use std::cell::RefCell;
use std::path::PathBuf;
use std::time::Instant;

use dfdu_tree::NodeRef;
use ratatui::crossterm::event::{KeyCode, KeyEvent};
use ratatui::style::Style;
use ratatui::text::{Line, Span};

use crate::app::{App, Effect, Selection};
use crate::fuzzy::Fuzzy;
use crate::input::{Edit, LineInput};
use crate::snapshot::{EntryData, Snapshot};

/// The query, its input line, and which entries of the current directory match it.
#[derive(Clone, Debug, Default)]
pub struct Filter {
    input: LineInput,
    /// The input has the keys.
    editing: bool,
    /// The query `hits` holds the matches of.
    query: String,
    /// `query` has words; without any, everything matches.
    active: bool,
    /// Matcher for `query`; a `RefCell` so drawing can highlight with it.
    fuzzy: RefCell<Fuzzy>,
    /// `hits[i]`: entry `i` matches `query`. Empty while the query has no words.
    hits: Vec<bool>,
    /// Directory the entries are from; another one clears the filter.
    dir: PathBuf,
    counts: Counts,
    /// The selected entry when the query hid it, and the entry the selection moved to then
    /// (`None`: no row was left). While that one stays selected, the selection goes back as
    /// soon as the hidden entry shows again.
    restore: Option<(NodeRef, Option<NodeRef>)>,
    /// Buffer for the matched character indices of a drawn name.
    indices: RefCell<Vec<u32>>,
}

/// Entries counted for the filter's line, ghosts left out. Kept up to date by
/// [`Filter::prepare`] only while the filter is active or being edited, and counted when it opens.
#[derive(Copy, Clone, Debug, Default)]
struct Counts {
    entries: usize,
    hidden: usize,
    matched: usize,
    matched_hidden: usize,
}

impl Filter {
    /// Brings the matches up to date with `snap.entries` before the rows are sorted or synced
    /// (not for frames whose rows stay as they are). `replaced`: the entries are not those of
    /// the last call with new ones appended (another directory, or a rescan replaced some), so
    /// what was computed per index no longer applies.
    pub fn prepare(&mut self, snap: &Snapshot, replaced: bool) {
        if snap.dir_path != self.dir {
            // Before the first snapshot there is no directory, and a query typed by then stays.
            if !self.dir.as_os_str().is_empty() {
                self.input.clear();
                self.editing = false;
                self.restore = None;
            }
            self.dir.clone_from(&snap.dir_path);
        }
        let fuzzy = self.fuzzy.get_mut();
        if self.input.text() != self.query {
            // Every word only grew or was added: whatever matches now matched before. The new
            // query has at least the words of the old one, so it is not empty either.
            let narrowed =
                !replaced && !fuzzy.is_empty() && self.input.text().starts_with(&self.query);
            self.query = self.input.text().to_owned();
            *fuzzy = Fuzzy::new(&self.query);
            self.active = !fuzzy.is_empty();
            if narrowed {
                for (hit, e) in self.hits.iter_mut().zip(&snap.entries) {
                    *hit = *hit && fuzzy.matches(&String::from_utf8_lossy(&e.name));
                }
            } else {
                self.hits.clear();
            }
        } else if replaced {
            self.hits.clear();
        }
        if self.active {
            self.hits.truncate(snap.entries.len());
            let from = self.hits.len();
            self.hits.extend(
                snap.entries[from..]
                    .iter()
                    .map(|e| fuzzy.matches(&String::from_utf8_lossy(&e.name))),
            );
        } else {
            self.hits.clear();
        }
        if self.active || self.editing {
            self.counts = self.count(&snap.entries);
        }
    }

    /// Counts `entries` for the filter's line.
    fn count(&self, entries: &[EntryData]) -> Counts {
        let mut c = Counts::default();
        for (i, e) in entries.iter().enumerate() {
            if e.ghost {
                continue;
            }
            let (hidden, hit) = (e.hidden(), self.matches(i, e));
            c.entries += 1;
            c.hidden += hidden as usize;
            c.matched += hit as usize;
            c.matched_hidden += (hidden && hit) as usize;
        }
        c
    }

    /// The query has words, so the entries that do not match it are hidden.
    pub fn active(&self) -> bool {
        self.active
    }

    /// Whether entry `index` (`e`) passes the filter; a lookup into what [`Filter::prepare`]
    /// computed.
    pub fn matches(&self, index: usize, _e: &EntryData) -> bool {
        !self.active || self.hits.get(index).copied().unwrap_or(false)
    }
}

/// Starts editing the filter, with the cursor after the query in force.
pub fn open(app: &mut App) {
    let f = &mut app.filter;
    f.editing = true;
    let text = f.input.text().to_owned();
    f.input.set(&text);
    f.counts = f.count(&app.snap.entries);
    app.keep_exact_order();
}

/// True while the filter input takes the keys.
pub fn editing(app: &App) -> bool {
    app.filter.editing
}

/// A key while editing: typing filters at once, `Enter` keeps the filter and gives the keys
/// back to the list, `Esc` clears it, `↑`/`↓` move the cursor in the list.
pub fn key(app: &mut App, k: KeyEvent, now: Instant) -> Vec<Effect> {
    match k.code {
        KeyCode::Up => app.move_by(-1),
        KeyCode::Down => app.move_by(1),
        _ => match app.filter.input.key(&k) {
            Edit::Changed => refilter(app, now),
            Edit::Submit => {
                app.filter.editing = false;
                if app.filter.input.text().trim().is_empty() {
                    clear(app);
                }
            }
            Edit::Cancel => {
                clear(app);
            }
            Edit::Moved | Edit::Ignored => {}
        },
    }
    Vec::new()
}

/// Applies a changed query to the rows at once, so the selection it moved is known: an entry
/// it hid is remembered, and one remembered comes back when it shows again.
fn refilter(app: &mut App, now: Instant) {
    let before = selected_node(app);
    app.request_refilter();
    app.reorder(now);
    let after = selected_node(app);
    let row_of = |app: &App, node| {
        app.rows
            .iter()
            .position(|&i| app.snap.entries[i].node == node)
    };
    app.filter.restore = match app.filter.restore {
        Some((hidden, moved_to)) if before == moved_to => match row_of(app, hidden) {
            Some(row) => {
                app.select(row);
                None
            }
            None => Some((hidden, after)),
        },
        _ => match before {
            Some(b) if after != before && row_of(app, b).is_none() => Some((b, after)),
            _ => None,
        },
    };
}

fn selected_node(app: &App) -> Option<NodeRef> {
    app.selected_entry().map(|e| e.node)
}

/// Clears an active filter. False if there was none. The entry the filter hid from the
/// selection is selected again, unless the user picked another one since.
pub fn clear(app: &mut App) -> bool {
    let f = &mut app.filter;
    let had = f.editing || !f.input.is_empty();
    f.editing = false;
    if f.input.is_empty() {
        return had;
    }
    f.input.clear();
    app.request_refilter();
    if let Some((hidden, moved_to)) = app.filter.restore.take()
        && selected_node(app) == moved_to
        && let Some(e) = app.snap.entries.iter().find(|e| e.node == hidden)
        && !e.ghost
        && !(app.hide_hidden && e.hidden())
    {
        app.selected = Some(Selection {
            node: Some(e.node),
            name: e.name.to_vec(),
            dev_ino: e.dev_ino,
        });
    }
    had
}

/// The filter's line above the status line, while it has one: the input while editing, else
/// the query in force.
pub fn line(app: &App) -> Option<Line<'static>> {
    let (f, t) = (&app.filter, &app.theme);
    if !f.editing && !f.active {
        return None;
    }
    let c = f.counts;
    let (shown, of) = if app.hide_hidden {
        (c.matched - c.matched_hidden, c.entries - c.hidden)
    } else {
        (c.matched, c.entries)
    };
    let spans = if f.editing {
        let mut spans = vec![Span::styled(" /", t.key), Span::raw(" ")];
        spans.extend(f.input.spans(Style::new(), t.key));
        spans.push(Span::styled(format!("   {shown} of {of}"), t.fg_dim));
        spans
    } else {
        vec![
            Span::styled(" filter: ", t.fg_dim),
            Span::raw(format!("\"{}\"", f.input.text())),
            Span::styled(format!(" ({shown} of {of})   "), t.fg_dim),
            Span::styled("/", t.key),
            Span::styled(" edit  ", t.fg_dim),
            Span::styled("Esc", t.key),
            Span::styled(" clear", t.fg_dim),
        ]
    };
    Some(Line::from(spans))
}

/// The name of entry `index` drawn in `style`, with the matched characters highlighted.
pub fn name_spans(app: &App, _index: usize, name: String, style: Style) -> Vec<Span<'static>> {
    let f = &app.filter;
    if !f.active {
        return vec![Span::styled(name, style)];
    }
    let mut indices = f.indices.borrow_mut();
    if f.fuzzy.borrow_mut().indices(&name, &mut indices).is_none() || indices.is_empty() {
        return vec![Span::styled(name, style)];
    }
    let matched = style.patch(app.theme.r#match);
    let mut spans = Vec::new();
    let mut run = String::new();
    let mut run_hit = false;
    let mut next = indices.iter().copied().peekable();
    for (i, ch) in name.chars().enumerate() {
        let hit = next.next_if_eq(&(i as u32)).is_some();
        if hit != run_hit && !run.is_empty() {
            let s = if run_hit { matched } else { style };
            spans.push(Span::styled(std::mem::take(&mut run), s));
        }
        run_hit = hit;
        run.push(ch);
    }
    if !run.is_empty() {
        spans.push(Span::styled(run, if run_hit { matched } else { style }));
    }
    spans
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use ratatui::crossterm::event::KeyModifiers;
    use ratatui::style::{Color, Modifier};

    use super::*;
    use crate::app::Overlay;
    use crate::snapshot::{EntryData, test_entry};
    use crate::tests::{
        assert_cue, press, render, render_buffer, snap_with_totals, style_at, styled, text,
    };
    use crate::theme::Theme;

    fn dir(i: u32, name: &str, size: u64) -> EntryData {
        EntryData {
            is_dir: true,
            ..test_entry(i, name, size)
        }
    }

    fn entries() -> Vec<EntryData> {
        let gib = 1 << 30;
        vec![
            dir(0, "Library", 112 * gib),
            dir(1, "dev", 78 * gib),
            dir(2, ".android", 51 * gib),
            dir(3, "Downloads", 9 * gib),
            test_entry(4, "notes.sqlite", 625 << 20),
            dir(5, "lib64", 300 << 20),
            test_entry(6, "build.log", 12 << 10),
        ]
    }

    fn app_with(theme: Theme, es: Vec<EntryData>, t0: Instant) -> App {
        let mut app = App::new(theme, t0);
        app.refresh(snap_with_totals(es, 1), t0);
        app
    }

    fn typed(app: &mut App, s: &str, now: Instant) {
        for c in s.chars() {
            assert!(app.key(press(KeyCode::Char(c)), now).is_empty());
        }
        app.reorder(now);
    }

    fn names(app: &App) -> Vec<String> {
        app.rows
            .iter()
            .map(|&i| String::from_utf8_lossy(&app.snap.entries[i].name).into_owned())
            .collect()
    }

    fn line_text(app: &App) -> Option<String> {
        line(app).map(|l| l.spans.iter().map(|s| s.content.as_ref()).collect())
    }

    #[test]
    fn typing_filters_and_highlights() {
        let t0 = Instant::now();
        let mut app = app_with(Theme::no_color(), entries(), t0);
        typed(&mut app, "/li", t0);
        assert!(editing(&app));
        assert_eq!(names(&app), ["Library", "notes.sqlite", "lib64"]);
        assert_eq!(line_text(&app).as_deref(), Some(" / li▌   3 of 7"));
        insta::assert_snapshot!("filter_editing_mono", render(&mut app, t0, 80, 9));
        let buf = render_buffer(&mut app, t0, 80, 9);
        let underlined = |s: Style| s.add_modifier.contains(Modifier::UNDERLINED);
        assert_cue(&buf, "Li", underlined);
        assert!(!underlined(style_at(&buf, "brary")));

        let mut app = app_with(Theme::ansi16(), entries(), t0);
        typed(&mut app, "/li", t0);
        let buf = render_buffer(&mut app, t0, 80, 9);
        insta::assert_snapshot!("filter_editing_ansi16", styled(&buf));
        let cyan_underlined = |s: Style| s.fg == Some(Color::Cyan) && underlined(s);
        assert_cue(&buf, "li", cyan_underlined);
        assert_cue(&buf, "3 of 7", |s| s.fg == Some(Color::DarkGray));
        // In a name, only the matched characters are highlighted.
        let row = text(&buf)
            .lines()
            .position(|l| l.contains("notes.sqlite"))
            .expect("row") as u16;
        let at = (0..68)
            .find(|&x| {
                (x..x + 12)
                    .map(|x| buf[(x, row)].symbol())
                    .collect::<String>()
                    == "notes.sqlite"
            })
            .expect("name");
        let hits: Vec<bool> = (at..at + 12)
            .map(|x| underlined(buf[(x, row)].style()))
            .collect();
        let want: Vec<bool> = "notes.sqlite"
            .chars()
            .enumerate()
            .map(|(i, _)| i == 8 || i == 9)
            .collect();
        assert_eq!(hits, want);
    }

    #[test]
    fn inactive_filter_counts_nothing_until_opened() {
        let t0 = Instant::now();
        let mut f = Filter::default();
        f.prepare(&snap_with_totals(entries(), 1), true);
        assert!(f.hits.is_empty());
        assert_eq!(f.counts.entries, 0);

        let mut app = app_with(Theme::no_color(), entries(), t0);
        assert_eq!(app.filter.counts.entries, 0);
        open(&mut app);
        assert_eq!(app.filter.counts.entries, 7);
        assert_eq!(app.filter.counts.matched, 7);
        assert_eq!(app.filter.counts.hidden, 1);
        assert_eq!(line_text(&app).as_deref(), Some(" / ▌   7 of 7"));
        typed(&mut app, "li", t0);
        assert_eq!(line_text(&app).as_deref(), Some(" / li▌   3 of 7"));
    }

    #[test]
    fn query_typed_before_the_first_snapshot_stays() {
        let t0 = Instant::now();
        let mut app = App::new(Theme::no_color(), t0);
        typed(&mut app, "/li", t0);
        app.refresh(snap_with_totals(entries(), 1), t0);
        app.reorder(t0);
        assert!(editing(&app));
        assert_eq!(names(&app), ["Library", "notes.sqlite", "lib64"]);
        assert_eq!(line_text(&app).as_deref(), Some(" / li▌   3 of 7"));

        let other = Snapshot {
            dir_path: "/elsewhere".into(),
            ..snap_with_totals(entries(), 2)
        };
        app.refresh(other, t0);
        app.reorder(t0);
        assert!(!editing(&app));
        assert_eq!(line_text(&app), None);
        assert_eq!(app.rows.len(), 7);
    }

    #[test]
    fn enter_keeps_filter_esc_clears() {
        let t0 = Instant::now();
        let mut app = app_with(Theme::no_color(), entries(), t0);
        typed(&mut app, "/dow", t0);
        app.key(press(KeyCode::Enter), t0);
        app.reorder(t0);
        assert!(!editing(&app));
        assert_eq!(names(&app), ["Downloads"]);
        assert_eq!(
            line_text(&app).as_deref(),
            Some(" filter: \"dow\" (1 of 7)   / edit  Esc clear")
        );
        // Keys go to the list again: `n` sorts by name instead of being typed.
        app.key(press(KeyCode::Char('n')), t0);
        app.reorder(t0);
        assert_eq!(app.sort.key, crate::order::SortKey::Name);
        assert_eq!(names(&app), ["Downloads"]);

        // `/` edits the query in force; Esc clears it and closes the input.
        typed(&mut app, "/n", t0);
        assert_eq!(line_text(&app).as_deref(), Some(" / down▌   1 of 7"));
        app.key(press(KeyCode::Esc), t0);
        app.reorder(t0);
        assert!(!editing(&app));
        assert_eq!(line_text(&app), None);
        assert_eq!(app.rows.len(), 7);

        // Ctrl-U empties the input; Enter on an empty query closes it.
        typed(&mut app, "/xyz", t0);
        assert!(app.rows.is_empty());
        app.key(KeyEvent::new(KeyCode::Char('u'), KeyModifiers::CONTROL), t0);
        app.reorder(t0);
        assert_eq!(app.rows.len(), 7);
        app.key(press(KeyCode::Enter), t0);
        assert_eq!(line_text(&app), None);
    }

    #[test]
    fn esc_in_list_clears_before_closing() {
        let t0 = Instant::now();
        let mut app = app_with(Theme::no_color(), entries(), t0);
        typed(&mut app, "/li", t0);
        app.key(press(KeyCode::Enter), t0);
        // An open overlay takes Esc first.
        app.key(press(KeyCode::Char('?')), t0);
        assert!(matches!(app.overlay, Overlay::Help(_)));
        app.key(press(KeyCode::Esc), t0);
        app.reorder(t0);
        assert!(matches!(app.overlay, Overlay::None));
        assert_eq!(app.rows.len(), 3);
        // Then Esc clears the filter, and a second Esc has nothing left to do.
        assert!(app.key(press(KeyCode::Esc), t0).is_empty());
        app.reorder(t0);
        assert_eq!(app.rows.len(), 7);
        assert_eq!(line_text(&app), None);
        assert!(!clear(&mut app));
    }

    #[test]
    fn hidden_toggle_and_filter_combine() {
        let t0 = Instant::now();
        let mut app = app_with(Theme::no_color(), entries(), t0);
        typed(&mut app, "/d", t0);
        assert_eq!(names(&app), ["dev", ".android", "Downloads", "build.log"]);
        app.key(press(KeyCode::Enter), t0);
        app.key(press(KeyCode::Char('e')), t0);
        app.reorder(t0);
        assert_eq!(names(&app), ["dev", "Downloads", "build.log"]);
        assert_eq!(
            line_text(&app).as_deref(),
            Some(" filter: \"d\" (3 of 6)   / edit  Esc clear")
        );
        app.key(press(KeyCode::Esc), t0);
        app.reorder(t0);
        assert_eq!(app.rows.len(), 6, "hidden files stay hidden");
    }

    #[test]
    fn new_entries_during_scan_are_filtered() {
        let t0 = Instant::now();
        let mut app = app_with(Theme::no_color(), entries(), t0);
        typed(&mut app, "/li", t0);
        app.key(press(KeyCode::Enter), t0);
        assert_eq!(app.rows.len(), 3);
        let mut es = entries();
        es.push(test_entry(7, "linux", 1 << 40));
        es.push(test_entry(8, "zzz", 1 << 40));
        let at = t0 + Duration::from_millis(100);
        app.refresh(snap_with_totals(es.clone(), 2), at);
        assert_eq!((app.rows.len(), app.new_count), (3, 1));
        assert_eq!(app.new_largest, 1 << 40);
        es.push(test_entry(9, "lilo.conf", 5));
        es.push(test_entry(10, "other", 5));
        app.refresh(snap_with_totals(es, 3), at);
        assert_eq!((app.rows.len(), app.new_count), (3, 2));
        assert_eq!(
            line_text(&app).as_deref(),
            Some(" filter: \"li\" (5 of 11)   / edit  Esc clear")
        );
        let frame = render(&mut app, at, 80, 9);
        assert!(frame.contains("+ 2 new entries since last sort"), "{frame}");
        app.key(press(KeyCode::Char('S')), at);
        app.reorder(at);
        assert_eq!(
            names(&app),
            ["linux", "Library", "notes.sqlite", "lib64", "lilo.conf"]
        );
    }

    #[test]
    fn filter_keeps_sort_order() {
        let t0 = Instant::now();
        let mut app = app_with(Theme::no_color(), entries(), t0);
        let with_o = |names: Vec<String>| -> Vec<String> {
            names.into_iter().filter(|n| n.contains('o')).collect()
        };
        let by_size = with_o(names(&app));
        assert_eq!(
            by_size,
            [".android", "Downloads", "notes.sqlite", "build.log"]
        );
        typed(&mut app, "/o", t0);
        assert_eq!(names(&app), by_size);
        app.key(press(KeyCode::Enter), t0);
        app.key(press(KeyCode::Char('n')), t0);
        app.reorder(t0);
        let filtered_by_name = names(&app);
        // Clearing the filter keeps the selected entry selected when the cursor was moved.
        app.key(press(KeyCode::Down), t0);
        app.key(press(KeyCode::Down), t0);
        let picked = app.selected_entry().map(|e| e.name.to_vec());
        app.key(press(KeyCode::Esc), t0);
        app.reorder(t0);
        assert_eq!(app.rows.len(), 7);
        assert_eq!(with_o(names(&app)), filtered_by_name);
        assert_eq!(app.selected_entry().map(|e| e.name.to_vec()), picked);
        assert_eq!(picked.as_deref(), Some(filtered_by_name[2].as_bytes()));
    }

    #[test]
    fn clearing_the_filter_selects_the_hidden_entry_again() {
        let t0 = Instant::now();
        let mut app = app_with(Theme::no_color(), entries(), t0);
        app.key(press(KeyCode::Down), t0);
        assert_eq!(app.selected_entry().map(|e| &e.name[..]), Some(&b"dev"[..]));
        // "dow" hides `dev`; the selection moves to a row that is left.
        typed(&mut app, "/dow", t0);
        assert_eq!(names(&app), ["Downloads"]);
        let picked = |app: &App| app.selected_entry().map(|e| e.name.to_vec());
        assert_eq!(picked(&app).as_deref(), Some(&b"Downloads"[..]));
        // While no other entry was picked, `dev` is selected again once it shows.
        app.key(press(KeyCode::Backspace), t0);
        assert_eq!(picked(&app).as_deref(), Some(&b"Downloads"[..]));
        app.key(press(KeyCode::Backspace), t0);
        assert_eq!(picked(&app).as_deref(), Some(&b"dev"[..]));
        typed(&mut app, "ow", t0);
        app.key(press(KeyCode::Esc), t0);
        app.reorder(t0);
        assert_eq!(picked(&app).as_deref(), Some(&b"dev"[..]));
        // An entry picked under the filter stays selected when the filter is cleared.
        typed(&mut app, "/o", t0);
        app.key(press(KeyCode::Enter), t0);
        app.key(press(KeyCode::Char('G')), t0);
        let last = picked(&app);
        assert_eq!(last.as_deref(), Some(&b"build.log"[..]));
        assert!(app.key(press(KeyCode::Esc), t0).is_empty());
        app.reorder(t0);
        assert_eq!(app.rows.len(), 7);
        assert_eq!(picked(&app), last);
    }

    /// The rows an exact sort gives of the entries the filter and the hidden toggle show.
    fn exact_rows(app: &App) -> Vec<usize> {
        let mut o = crate::order::Order::default();
        let visible = |i: usize, e: &EntryData| {
            !e.ghost && !(app.hide_hidden && e.hidden()) && app.filter.matches(i, e)
        };
        o.sort_exact(&app.snap.entries, visible, app.sort);
        o.rows().to_vec()
    }

    #[test]
    fn filter_rows_are_those_of_an_exact_sort() {
        let t0 = Instant::now();
        let mut es = entries();
        // Equal sizes, so the order of these comes from the tie-breaks.
        es.push(test_entry(7, "dove", 9 << 30));
        es.push(test_entry(8, "Dot", 9 << 30));
        let mut app = app_with(Theme::no_color(), es.clone(), t0);
        let steps: [&[KeyCode]; 7] = [
            &[KeyCode::Char('/'), KeyCode::Char('o')],
            &[KeyCode::Char('w')],
            &[KeyCode::Backspace],
            &[KeyCode::Backspace],
            &[KeyCode::Char('d')],
            &[KeyCode::Char('z')],
            &[KeyCode::Esc],
        ];
        let mut seen = Vec::new();
        for keys in steps {
            for &k in keys {
                app.key(press(k), t0);
            }
            app.reorder(t0);
            assert_eq!(app.rows, exact_rows(&app), "after {keys:?}");
            seen.push(app.rows.len());
        }
        assert_eq!(seen, [6, 1, 6, 9, 6, 0, 9]);

        // During a scan, an entry that arrived since the last sort is sorted in with the rest.
        typed(&mut app, "/do", t0);
        es.push(test_entry(9, "docs", 10 << 30));
        app.refresh(snap_with_totals(es, 2), t0);
        assert_eq!(app.new_count, 1);
        typed(&mut app, "s", t0);
        assert_eq!(names(&app), ["docs", "Downloads"]);
        assert_eq!(app.rows, exact_rows(&app));
        assert_eq!(app.new_count, 0);
    }

    #[test]
    fn a_snapshot_before_the_cleared_filter_is_applied_is_sorted_in() {
        let t0 = Instant::now();
        let mut app = app_with(Theme::no_color(), entries(), t0);
        typed(&mut app, "/d", t0);
        app.key(press(KeyCode::Esc), t0);
        // The scan's next snapshot arrives before the loop applies the cleared filter.
        let mut es = entries();
        es[6].size = 200 << 30;
        es.push(test_entry(7, "zeta", 1 << 30));
        app.refresh(snap_with_totals(es, 2), t0);
        app.reorder(t0);
        assert_eq!(app.rows, exact_rows(&app));
        assert_eq!(names(&app)[0], "build.log");
        assert_eq!(app.rows.len(), 8);
    }

    #[test]
    fn showing_hidden_files_before_the_cleared_filter_is_applied_shows_them() {
        let t0 = Instant::now();
        let mut app = app_with(Theme::no_color(), entries(), t0);
        app.key(press(KeyCode::Char('e')), t0);
        app.reorder(t0);
        assert_eq!(app.rows.len(), 6);
        typed(&mut app, "/d", t0);
        app.key(press(KeyCode::Esc), t0);
        app.key(press(KeyCode::Char('e')), t0);
        app.reorder(t0);
        assert_eq!(app.rows, exact_rows(&app));
        assert_eq!(app.rows.len(), 7);
    }

    #[test]
    fn filter_rows_after_an_automatic_sort_are_those_of_an_exact_sort() {
        let t0 = Instant::now();
        let es = |x: u64| vec![test_entry(0, "x", x), test_entry(1, "y", 1_001_000)];
        let mut app = app_with(Theme::no_color(), es(1_000_000), t0);
        assert_eq!(names(&app), ["y", "x"]);
        // `x` overtakes `y` by less than 2%: the automatic sort keeps them where they are.
        let later = t0 + Duration::from_secs(5);
        app.refresh(snap_with_totals(es(1_003_000), 2), later);
        app.reorder(later);
        assert_eq!(names(&app), ["y", "x"]);
        app.key(press(KeyCode::Char('/')), later);
        app.reorder(later);
        assert_eq!(names(&app), ["y", "x"], "opening the filter moves nothing");
        // Any change of the query gives the rows of an exact sort, like `S`.
        typed(&mut app, " ", later);
        assert_eq!(names(&app), ["x", "y"]);
        typed(&mut app, "x", later);
        assert_eq!(names(&app), ["x"]);
        app.key(press(KeyCode::Backspace), later);
        app.reorder(later);
        assert_eq!(names(&app), ["x", "y"]);
        assert_eq!(app.rows, exact_rows(&app));
    }

    #[test]
    fn non_utf8_names_match_as_displayed() {
        let t0 = Instant::now();
        let raw = EntryData {
            name: b"caf\xe9.txt"[..].into(),
            ..test_entry(0, "", 30)
        };
        let es = vec![raw, test_entry(1, "Café", 20), test_entry(2, "cfg", 10)];
        let mut app = app_with(Theme::no_color(), es, t0);
        typed(&mut app, "/caf", t0);
        assert_eq!(names(&app), ["caf\u{fffd}.txt", "Café"]);
        // The replacement character is matched like the others, and highlighted in place.
        app.key(press(KeyCode::Esc), t0);
        typed(&mut app, "/\u{fffd}t", t0);
        assert_eq!(names(&app), ["caf\u{fffd}.txt"]);
        let buf = render_buffer(&mut app, t0, 60, 6);
        assert_cue(&buf, "\u{fffd}", |s| {
            s.add_modifier.contains(Modifier::UNDERLINED)
        });
        assert!(
            !style_at(&buf, ".txt")
                .add_modifier
                .contains(Modifier::UNDERLINED)
        );
    }

    /// `n` entries with names like `data-0000042-report.json`.
    fn synthetic(n: usize) -> Snapshot {
        const WORDS: [&str; 8] = [
            "cache",
            "build",
            "report",
            "data",
            "node_modules",
            "image",
            "Video",
            "tmp",
        ];
        const EXTS: [&str; 5] = ["json", "log", "png", "rs", "tar.gz"];
        let base = test_entry(0, "x", 1);
        let entries = (0..n)
            .map(|i| EntryData {
                name: format!(
                    "{}-{i:07}-{}.{}",
                    WORDS[i % 8],
                    WORDS[(i / 8) % 8],
                    EXTS[i % 5]
                )
                .as_bytes()
                .into(),
                ..base.clone()
            })
            .collect();
        Snapshot {
            entries,
            ..Default::default()
        }
    }

    /// Times `prepare` for a one-letter query and then for a second letter appended.
    fn prepare_times(n: usize) {
        let snap = synthetic(n);
        let mut f = Filter::default();
        f.prepare(&snap, true);
        let mut times = Vec::new();
        for q in ["e", "ep"] {
            f.input.set(q);
            let t = Instant::now();
            f.prepare(&snap, false);
            times.push(t.elapsed());
        }
        let hits = f.hits.iter().filter(|&&h| h).count();
        println!("prepare on {n} entries: {times:?}, {hits} match \"ep\"");
        assert!(hits > 0 && hits < n);
        for t in times {
            assert!(t < Duration::from_millis(250), "{t:?}");
        }
    }

    /// Times the first keystrokes of a filter in a directory of `n` entries of different sizes
    /// whose last sort was automatic, as when its scan finished, as the run loop handles them,
    /// each with its frame: `/`, which keeps an exact sort of the entries, then `e`, which
    /// prepares the filter and picks its rows from that sort.
    fn first_keystroke_times(n: usize) {
        let t0 = Instant::now();
        let mut es = synthetic(n).entries;
        for (i, e) in es.iter_mut().enumerate() {
            e.size = (i as u64 * 7919) % 1_000_003;
        }
        let mut app = app_with(Theme::no_color(), es.clone(), t0);
        let later = t0 + Duration::from_secs(5);
        app.refresh(snap_with_totals(es, 2), later);
        render_buffer(&mut app, later, 100, 30);
        let mut times = Vec::new();
        for c in ['/', 'e'] {
            let t = Instant::now();
            app.key(press(KeyCode::Char(c)), later);
            app.reorder(later);
            render_buffer(&mut app, later, 100, 30);
            times.push(t.elapsed());
        }
        let shown = app.rows.len();
        println!(
            "first filter keystrokes on {n} entries, each with its frame: {times:?}, {shown} rows"
        );
        assert!(shown > 0 && shown < n);
        for t in times {
            assert!(t < Duration::from_millis(50), "{t:?}");
        }
    }

    #[test]
    fn filtering_a_large_directory_is_fast() {
        prepare_times(100_000);
    }

    #[test]
    #[ignore = "builds a million entries; run in release. On an M4 Max, release: prepare 21-23 \
                ms; with the frame, `/` 32-34 ms and `e` 29-33 ms. Debug: `/` 0.59 s, `e` 0.48 s"]
    fn filtering_a_large_directory_is_fast_at_a_million_entries() {
        first_keystroke_times(1_000_000);
        prepare_times(1_000_000);
    }
}
