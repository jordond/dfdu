//! Drawing the view: the progress of a comparison, then the summary with the keep rule, and the
//! groups. A group's header names the directory all its files are in; each file's row shows the
//! rest of its path, shortened in the middle, `keep` on the copy marking leaves, and its date in a
//! column at the right edge.

use std::path::{Component, Path, PathBuf};
use std::sync::atomic::Ordering;

use dfdu_dupes::{Group, Progress};
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use super::{Row, kept, live, min_size, rows};
use crate::app::App;
use crate::theme::{count, human, truncate_middle};

/// `path` relative to `root`, or whole when it is not below it.
fn shown(path: &Path, root: &Path) -> PathBuf {
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
        .map(|(i, c)| (i, shown(&c.path, root)))
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

/// The label of the copy marking leaves, in a column of its own before the dates.
const KEEP: &str = "  keep";

/// `s` cut to `w` cells, ending in `…` when it was cut.
fn clip_end(s: &str, w: usize) -> String {
    if Span::raw(s).width() <= w {
        return s.to_owned();
    }
    let mut out = String::new();
    let mut used = 0;
    for c in s.chars() {
        let cw = Span::raw(c.to_string()).width();
        if used + cw + 1 > w {
            break;
        }
        out.push(c);
        used += cw;
    }
    if w > 0 {
        out.push('…');
    }
    out
}

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

pub fn draw(f: &mut Frame, app: &mut App, area: Rect) {
    let t = app.theme.clone();
    let height = area.height as usize;
    app.list_height = height.saturating_sub(1);
    let Some(run) = app.dupes.run.clone() else {
        let again = crate::app::keys::first_key(crate::app::keys::Action::CompareAgain);
        let msg = format!(" duplicates: not compared yet ({again} compares the files of the scan)");
        f.render_widget(Paragraph::new(Line::styled(msg, t.fg_dim)), area);
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
    let wasted: u64 = rows
        .iter()
        .filter_map(|r| match r {
            Row::Group(g) => Some(found.groups[*g].wasted()),
            Row::File(..) => None,
        })
        .sum();
    let groups = rows.iter().filter(|r| matches!(r, Row::Group(_))).count();
    let mut summary = format!(
        " {} of identical files of {} or more · up to {} to free · {} files compared",
        match groups {
            1 => "1 group".to_string(),
            n => format!("{} groups", count(n as u64)),
        },
        human(min_size()),
        human(wasted),
        count(found.compared)
    );
    if found.skipped > 0 {
        summary.push_str(" · ");
        summary.push_str(&count(found.skipped));
        summary.push_str(" skipped");
    }
    // The keep rule at the right edge, where a narrow terminal does not cut it.
    let keep = format!("  keep: {} ", app.dupes.keep.label());
    let room = (area.width as usize).saturating_sub(Span::raw(&keep).width());
    let summary = clip_end(&summary, room);
    let pad = room.saturating_sub(Span::raw(&summary).width());
    let mut lines = vec![Line::from(vec![
        Span::styled(summary, t.fg_dim),
        Span::raw(" ".repeat(pad)),
        Span::styled(keep, t.fg_dim),
    ])];
    if rows.is_empty() {
        lines.push(Line::raw(" no duplicates"));
    }
    let fit = height.saturating_sub(1).max(1);
    let d = &mut app.dupes;
    d.cursor = d.cursor.min(rows.len().saturating_sub(1));
    d.scroll = d
        .scroll
        .min(d.cursor)
        .max((d.cursor + 1).saturating_sub(fit));
    let (cursor, scroll) = (d.cursor, d.scroll);
    let width = area.width as usize;
    // The shared directory and the rest of each path, for the groups on screen.
    let mut paths: Vec<Option<GroupPaths>> = vec![None; found.groups.len()];
    for row in rows.iter().skip(scroll).take(fit) {
        let (Row::Group(g) | Row::File(g, _)) = *row;
        if paths[g].is_none() {
            paths[g] = Some(group_paths(app, &found.groups[g], &run.root));
        }
    }
    for (i, row) in rows.iter().enumerate().skip(scroll).take(fit) {
        let (Row::Group(group) | Row::File(group, _)) = *row;
        let Some((dir, files)) = &paths[group] else {
            continue;
        };
        let mut line = match *row {
            Row::Group(_) => {
                let gr = &found.groups[group];
                let free = if gr.all_clones() {
                    "clones: deleting copies frees nothing".to_string()
                } else {
                    format!("{} to free", human(gr.wasted()))
                };
                let n = files.len();
                let mut spans = vec![
                    Span::styled(format!(" {n} copies of {}", human(gr.size)), t.title),
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
            Row::File(_, file) => {
                let c = &found.groups[group].files[file];
                let marked = live(app, c).is_some_and(|n| app.marks.contains(n));
                let mark = if marked { "●" } else { " " };
                let date = dfdu_store::history::date_time(c.mtime);
                let rest = (files.iter())
                    .find(|(i, _)| *i == file)
                    .map(|(_, p)| p.display().to_string())
                    .unwrap_or_default();
                let lead = Span::styled(format!("   {mark} "), t.key);
                let keep = if kept(app, &found.groups[group]) == Some(file) {
                    Span::styled(KEEP, t.status_ok)
                } else {
                    Span::raw(" ".repeat(KEEP.len()))
                };
                let date = Span::styled(format!("  {date}"), t.fg_dim);
                let room = width.saturating_sub(lead.width() + keep.width() + date.width());
                let path = truncate_middle(&rest, room);
                let pad = room.saturating_sub(Span::raw(&path).width());
                Line::from(vec![
                    lead,
                    Span::raw(path),
                    Span::raw(" ".repeat(pad)),
                    keep,
                    date,
                ])
            }
        };
        if i == cursor {
            line = line.style(t.cursor);
        }
        lines.push(line);
    }
    f.render_widget(Paragraph::new(lines), area);
}
