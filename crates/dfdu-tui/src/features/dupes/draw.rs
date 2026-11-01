//! Drawing the view: the progress of a comparison, then the review layout (`features::review`):
//! the summary bar, the keep rule with every rule listed and the current one highlighted, the
//! column names, and the groups. A group's header says how many copies, what deleting them
//! frees and the directory all its files are in; clones, which free nothing, are dimmed. Each
//! file's row has its checkbox (`KEEP` on the copy the keep rule keeps), size, age and the rest
//! of its path. The detail pane shows the selected copy, which one is kept and which go, and
//! whether deleting frees anything.

use std::path::{Component, Path, PathBuf};
use std::sync::atomic::Ordering;

use dfdu_dupes::{Candidate, Found, Group, Progress};
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::Modifier;
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use super::{Keep, Row, kept, live, min_size, open, rows};
use crate::app::App;
use crate::app::keys::{Action, first_key};
use crate::features::review::{self, Tally, Tick};
use crate::format::{count, human, plural, truncate_middle};
use crate::model::outside::tilde;
use crate::theme::Theme;
use crate::view::columns::cursor;
use crate::view::empty::{self, Empty};

/// `path` relative to `root`, or whole when it is not below it.
fn relative_to_root(path: &Path, root: &Path) -> PathBuf {
    match path.strip_prefix(root) {
        Ok(rel) if !rel.as_os_str().is_empty() => rel.to_path_buf(),
        _ => path.to_path_buf(),
    }
}

/// The directory all of `paths` are in, as deep as it goes; empty when they share none.
fn shared_dir(paths: &[PathBuf]) -> PathBuf {
    fn dir(p: &Path) -> Vec<Component<'_>> {
        p.parent()
            .map(|d| d.components().collect())
            .unwrap_or_default()
    }
    let Some(first) = paths.first() else {
        return PathBuf::new();
    };
    let mut shared = dir(first);
    for p in &paths[1..] {
        let same = shared.iter().zip(dir(p)).take_while(|(a, b)| **a == *b);
        let n = same.count();
        shared.truncate(n);
    }
    shared.iter().collect()
}

/// The directory a group's files share, and the rest of each file's path by its index.
type GroupPaths = (PathBuf, Vec<(usize, PathBuf)>);

/// The paths of `group`'s files still in the tree as the view shows them: below the scan root
/// `root`, and below the directory they share, which is returned with them.
fn group_paths(app: &App, group: &Group, root: &Path) -> GroupPaths {
    let paths: Vec<(usize, PathBuf)> = (group.files.iter().enumerate())
        .filter(|(_, c)| live(app, c).is_some())
        .map(|(i, c)| (i, relative_to_root(&c.path, root)))
        .collect();
    let all: Vec<PathBuf> = paths.iter().map(|(_, p)| p.clone()).collect();
    let dir = shared_dir(&all);
    let rest = (paths.into_iter())
        .map(|(i, p)| {
            let below = p.strip_prefix(&dir).map(Path::to_path_buf).unwrap_or(p);
            (i, below)
        })
        .collect();
    (dir, rest)
}

/// Cells of the size and age columns.
const SIZE: usize = 9;
const AGE: usize = 8;
/// Before a file's checkbox, under its group's `▾`.
const INDENT: &str = "   ";

fn progress_line(p: &Progress) -> String {
    let get = |a: &std::sync::atomic::AtomicU64| a.load(Ordering::Relaxed);
    let what = match get(&p.round) {
        0 | 1 => "comparing the starts and ends of files",
        _ => "comparing the contents of files",
    };
    format!(
        " {what}: {} of {} files, {} of {}",
        count(get(&p.files)),
        count(get(&p.round_files)),
        human(get(&p.bytes)),
        human(get(&p.round_bytes)),
    )
}

/// What the summary bar counts: every copy still in the tree that deleting would free
/// something for, but the kept ones unless they are ticked.
fn tally(app: &App, found: &Found) -> (Tally, usize) {
    let mut t = Tally::default();
    let mut groups = 0;
    for group in &found.groups {
        let left = group
            .files
            .iter()
            .filter(|c| live(app, c).is_some())
            .count();
        if left < 2 {
            continue;
        }
        groups += 1;
        if group.all_clones() {
            continue;
        }
        let keep = kept(app, group);
        for (i, c) in group.files.iter().enumerate() {
            let Some(node) = live(app, c) else {
                continue;
            };
            let ticked = app.session.marks.contains(node);
            if Some(i) != keep || ticked {
                t.add(c.freed(), ticked);
            }
        }
    }
    (t, groups)
}

/// The keep rule: every rule, the one in force highlighted, and the key that changes it.
fn keep_line(t: &Theme, keep: Keep) -> Line<'static> {
    let mut spans = vec![Span::styled(" keep ", t.fg_dim)];
    for (i, rule) in Keep::ALL.into_iter().enumerate() {
        if i > 0 {
            spans.push(Span::styled(" · ", t.fg_dim));
        }
        spans.push(if rule == keep {
            Span::styled(rule.label(), t.key.add_modifier(Modifier::REVERSED))
        } else {
            Span::styled(rule.label(), t.fg_dim)
        });
    }
    spans.push(Span::styled(
        format!("   {} change", first_key(Action::DupesKeep)),
        t.fg_dim,
    ));
    Line::from(spans)
}

/// What the view does, the first line of its empty states.
fn what_it_does() -> String {
    format!(
        "Finds files with identical contents anywhere in the scan (files of {} or more).",
        human(min_size())
    )
}

/// Before anything was compared: how many files a comparison would read, and the key that
/// starts it.
fn not_compared(f: &mut Frame, app: &App, area: Rect) {
    let back = (first_key(Action::Close), "back to Files");
    let (why, keys) = match (app.session.snap.busy, app.session.dupes.candidates) {
        _ if !app.session.outside.local => (
            "It reads this machine's files, and this scan is remote or from a file: there is \
             nothing here to compare."
                .to_owned(),
            vec![back],
        ),
        (true, _) => (
            "The scan is still running: compare once it is done.".to_owned(),
            vec![back],
        ),
        (false, None) => ("Counting the files to compare…".to_owned(), vec![back]),
        (false, Some(0)) => (
            "No two files that large have the same size: there is nothing to compare.".into(),
            vec![back],
        ),
        (false, Some(n)) => (
            format!(
                "{} files share their size with another. Nothing is compared until you ask: \
                 comparing reads the files and can take a while.",
                count(n as u64)
            ),
            vec![(first_key(Action::Enter), "compare now"), back],
        ),
    };
    let e = Empty {
        title: "Duplicates",
        text: vec![what_it_does(), why],
        keys,
    };
    empty::draw(f, &app.theme, area, &e);
}

pub fn draw(f: &mut Frame, app: &mut App, area: Rect) {
    let t = app.theme.clone();
    let Some(run) = app.session.dupes.run.clone() else {
        not_compared(f, app, area);
        return;
    };
    let Some(found) = run.result.get() else {
        f.render_widget(
            Paragraph::new(Line::styled(progress_line(&run.progress), t.fg_dim)),
            area,
        );
        return;
    };
    let rows = rows(app);
    if rows.is_empty() {
        let e = Empty {
            title: "Duplicates",
            text: vec![
                what_it_does(),
                format!(
                    "No duplicates among the {} files compared.",
                    count(found.compared)
                ),
            ],
            keys: vec![
                (first_key(Action::CompareAgain), "compare again"),
                (first_key(Action::Close), "back to Files"),
            ],
        };
        empty::draw(f, &t, area, &e);
        return;
    }
    let (sum, groups) = tally(app, found);
    let mut extra = format!(" · {} group{}", count(groups as u64), plural(groups as u64));
    if found.skipped > 0 {
        extra.push_str(&format!(" · {} skipped", count(found.skipped)));
    }
    let head = vec![
        review::summary(app, sum, "copies", &extra, area.width),
        keep_line(&t, app.session.dupes.keep),
    ];
    let head_h = crate::view::cells(head.len());
    let body = Rect {
        y: area.y + head_h.min(area.height),
        height: area.height.saturating_sub(head_h),
        ..area
    };
    f.render_widget(Paragraph::new(head), area);
    let (list, pane) = review::split(body, 5);
    let fit = usize::from(list.height).saturating_sub(1).max(1);
    app.list_height = fit;
    let at = &mut app.session.dupes.cursor;
    *at = (*at).min(rows.len().saturating_sub(1));
    let d = &mut app.dupes;
    d.scroll = d.scroll.min(*at).max((*at + 1).saturating_sub(fit));
    let (cursor_at, scroll) = (*at, d.scroll);
    let app = &*app;
    let width = usize::from(list.width);
    let headings = format!(
        "{INDENT}{:w$} {:>SIZE$}  {:>AGE$}  path",
        "",
        "size",
        "modified",
        w = review::CHECK_WIDTH
    );
    let mut lines = vec![Line::styled(headings, t.fg_dim)];
    // The shared directory and the rest of each path, for the groups on screen.
    let mut paths: Vec<Option<GroupPaths>> = vec![None; found.groups.len()];
    for row in rows.iter().skip(scroll).take(fit) {
        let (Row::Group(g) | Row::File(g, _)) = *row;
        if paths[g].is_none() {
            paths[g] = Some(group_paths(app, &found.groups[g], &run.root));
        }
    }
    let now = dfdu_tree::now_secs();
    for (i, row) in rows.iter().enumerate().skip(scroll).take(fit) {
        let (Row::Group(group) | Row::File(group, _)) = *row;
        let Some((dir, files)) = &paths[group] else {
            continue;
        };
        let gr = &found.groups[group];
        let line = match *row {
            Row::Group(_) => group_line(app, group, gr, dir, files.len(), width),
            Row::File(_, file) => {
                let rest = (files.iter())
                    .find(|(i, _)| *i == file)
                    .map(|(_, p)| p.display().to_string())
                    .unwrap_or_default();
                file_line(app, gr, file, &rest, now, width)
            }
        };
        lines.push(if i == cursor_at {
            cursor(&t, line, list.width)
        } else {
            line
        });
    }
    f.render_widget(Paragraph::new(lines), list);
    if let Some(pane) = pane {
        let detail = detail_lines(app, found, rows.get(cursor_at), &run.root);
        review::draw_detail(f, &t, pane, review::on_right(body), detail);
    }
}

/// A group's header: `▾ 3 copies of 1.0 MiB · frees 2.0 MiB · in photos/`; clones dimmed.
fn group_line(
    app: &App,
    g: usize,
    group: &Group,
    dir: &Path,
    n: usize,
    width: usize,
) -> Line<'static> {
    let t = &app.theme;
    let clones = group.all_clones();
    let arrow = if open(app, g, group) {
        " ▾ "
    } else {
        " ▸ "
    };
    let (what, free) = if clones {
        (
            "clones",
            "frees nothing (clones share their data)".to_owned(),
        )
    } else {
        ("copies", format!("frees {}", human(group.wasted())))
    };
    let style = if clones { t.fg_dim } else { t.title };
    let mut spans = vec![
        Span::raw(arrow),
        Span::styled(format!("{n} {what} of {}", human(group.size)), style),
        Span::styled(format!(" · {free}"), t.fg_dim),
    ];
    if !dir.as_os_str().is_empty() {
        spans.push(Span::styled(" · in ", t.fg_dim));
        let used: usize = spans.iter().map(Span::width).sum();
        let dir = format!("{}{}", dir.display(), std::path::MAIN_SEPARATOR);
        spans.push(Span::raw(truncate_middle(&dir, width.saturating_sub(used))));
    }
    Line::from(spans)
}

/// A file's row: its checkbox, size, age at `now` and `rest`, its path below the group's
/// directory, in `width` cells.
fn file_line(
    app: &App,
    group: &Group,
    file: usize,
    rest: &str,
    now: i64,
    width: usize,
) -> Line<'static> {
    let t = &app.theme;
    let c = &group.files[file];
    let ticked = live(app, c).is_some_and(|n| app.session.marks.contains(n));
    let tick = match (ticked, kept(app, group) == Some(file)) {
        (true, _) => Tick::On,
        (false, true) => Tick::Keep,
        (false, false) => Tick::Off,
    };
    let mut spans = vec![
        Span::raw(INDENT),
        review::checkbox(t, tick),
        Span::raw(format!(" {:>SIZE$}", human(c.size))),
        Span::styled(format!("  {:>AGE$}  ", review::age(c.mtime, now)), t.fg_dim),
    ];
    let used: usize = spans.iter().map(Span::width).sum();
    let style = if group.all_clones() {
        t.fg_dim
    } else {
        ratatui::style::Style::new()
    };
    spans.extend(review::path(t, rest, 0, width.saturating_sub(used), style));
    Line::from(spans)
}

/// What deleting copy `c` frees, in words.
fn frees(c: &Candidate) -> String {
    if c.links > 0 {
        format!(
            "has {} other name{} (hard links): deleting this one frees nothing",
            c.links,
            plural(u64::from(c.links))
        )
    } else {
        match c.private {
            Some(0) => "a clone: shares all its data, deleting it frees nothing".into(),
            Some(p) if p < c.alloc => format!("shares some data: deleting it frees {}", human(p)),
            _ => format!("deleting it frees {}", human(c.freed())),
        }
    }
}

/// The detail pane: the selected copy, the copy kept and those that go, and what deleting the
/// selected one frees. On a group's row, its copies.
fn detail_lines(app: &App, found: &Found, row: Option<&Row>, root: &Path) -> Vec<Line<'static>> {
    let t = &app.theme;
    let Some(&row) = row else {
        return Vec::new();
    };
    let (Row::Group(g) | Row::File(g, _)) = row;
    let group = &found.groups[g];
    let home = app.session.home.as_deref();
    let name = |c: &Candidate| tilde(&dfdu_tree::HostPath::local(&c.path), home);
    let mut out = Vec::new();
    if let Row::File(_, i) = row {
        let c = &group.files[i];
        out.push(Line::styled(name(c), t.title));
        let modified = dfdu_tree::date_time(c.mtime);
        out.push(Line::styled(
            format!("{} · modified {modified}", human(c.size)),
            t.fg_dim,
        ));
        out.push(Line::styled(frees(c), t.fg_dim));
        out.push(Line::raw(""));
    }
    let keep = kept(app, group);
    for (i, c) in group.files.iter().enumerate() {
        if live(app, c).is_none() {
            continue;
        }
        let rel = relative_to_root(&c.path, root).display().to_string();
        let (label, style) = if Some(i) == keep {
            ("kept", t.status_ok)
        } else {
            ("goes", t.fg_dim)
        };
        out.push(Line::from(vec![
            Span::styled(format!("{label} "), style),
            Span::raw(rel),
        ]));
    }
    out
}

#[cfg(test)]
mod tests;
