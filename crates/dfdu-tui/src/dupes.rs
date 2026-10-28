//! The duplicates view (`4`): files with identical contents below the scan root, found on
//! request with [`dfdu_dupes`] once the scan is done.
//!
//! Switching the view on starts the comparison in a thread of its own, from the candidates read
//! under the read lock ([`Effect::FindDupes`]); the view shows its progress and then the groups,
//! the most space to free first. Marks are the list's: `Space` marks a copy, or on a group every
//! copy but the oldest, `a` does that for every group, and `d` and `D` delete what is marked
//! through the usual dialog, which checks each file against the scan. Groups of APFS clones,
//! which free nothing, are listed but `a` leaves them alone.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock};

use dfdu_dupes::{Candidate, Found, Group, Options, Progress};
use dfdu_engine::Model;
use dfdu_tree::{Kind, NodeRef};
use ratatui::Frame;
use ratatui::crossterm::event::{KeyCode, KeyEvent};
use ratatui::layout::Rect;
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use crate::app::{App, Effect};
use crate::marks;
use crate::snapshot::{Ident, Target};
use crate::theme::{count, human};

/// Files smaller than this are not compared.
pub const MIN_SIZE: u64 = 1 << 20;

/// What the view keeps between frames.
#[derive(Clone, Debug, Default)]
pub struct Dupes {
    pub on: bool,
    cursor: usize,
    scroll: usize,
    run: Option<Run>,
    /// The node of each file of the result, by path, found under the read lock; `None` once a
    /// file is gone from the tree (deleted).
    nodes: HashMap<PathBuf, Option<NodeRef>>,
}

/// A comparison, running or done.
#[derive(Clone, Debug)]
struct Run {
    root: PathBuf,
    progress: Arc<Progress>,
    cancel: Arc<AtomicBool>,
    result: Arc<OnceLock<Found>>,
}

pub fn active(app: &App) -> bool {
    app.dupes.on
}

/// A comparison is running: the view's progress line needs frames.
pub fn running(app: &App) -> bool {
    app.dupes.on
        && app
            .dupes
            .run
            .as_ref()
            .is_some_and(|r| r.result.get().is_none())
}

fn found(app: &App) -> Option<&Found> {
    app.dupes.run.as_ref()?.result.get()
}

/// Switches the view on or off. On, it starts a comparison unless one ran already.
pub fn show(app: &mut App, on: bool) -> Vec<Effect> {
    app.dupes.on = on;
    if !on {
        return Vec::new();
    }
    crate::treemap::show(app, false);
    crate::cleanup::show(app, false);
    if app.dupes.run.is_none() {
        return start(app);
    }
    Vec::new()
}

fn start(app: &mut App) -> Vec<Effect> {
    if app.snap.busy {
        app.status = Some("duplicates: wait until the scan is done, then press r".into());
        return Vec::new();
    }
    app.dupes.cursor = 0;
    app.dupes.scroll = 0;
    app.dupes.nodes.clear();
    vec![Effect::FindDupes]
}

/// Starts comparing `cands` (read under the lock by the run loop) in a thread.
pub fn begin(app: &mut App, root: PathBuf, cands: Vec<Candidate>) {
    let run = Run {
        root,
        progress: Arc::default(),
        cancel: Arc::default(),
        result: Arc::default(),
    };
    let (progress, cancel, result) = (run.progress.clone(), run.cancel.clone(), run.result.clone());
    let spawned = std::thread::Builder::new()
        .name("dfdu-dupes".into())
        .spawn(move || {
            let opts = Options {
                min_size: MIN_SIZE,
                threads: 4,
            };
            let _ = result.set(dfdu_dupes::find(cands, &opts, &progress, &cancel));
        });
    match spawned {
        Ok(_) => app.dupes.run = Some(run),
        Err(e) => app.status = Some(format!("duplicates: cannot start a thread: {e}")),
    }
}

/// The view needs a read: a result whose files have no nodes yet.
pub fn stale(app: &App) -> bool {
    app.dupes.on
        && found(app).is_some_and(|f| {
            f.groups
                .iter()
                .flat_map(|g| &g.files)
                .any(|c| !app.dupes.nodes.contains_key(&c.path))
        })
}

/// Under the read lock: the node of every file of the result, found once by path and then
/// checked to still be in the tree.
pub fn read(m: &Model, app: &App) -> HashMap<PathBuf, Option<NodeRef>> {
    let Some(f) = found(app).filter(|_| app.dupes.on) else {
        return HashMap::new();
    };
    f.groups
        .iter()
        .flat_map(|g| &g.files)
        .map(|c| {
            let node = match app.dupes.nodes.get(&c.path) {
                Some(Some(n)) => m.tree.resolve_visible(*n).map(|_| *n),
                Some(None) => None,
                None => crate::snapshot::find_path(&m.tree, &c.path)
                    .filter(|&ix| m.tree.dev_ino(ix) == c.dev_ino)
                    .map(|ix| m.tree.node_ref(ix)),
            };
            (c.path.clone(), node)
        })
        .collect()
}

pub fn apply(app: &mut App, nodes: HashMap<PathBuf, Option<NodeRef>>) {
    if !nodes.is_empty() {
        app.dupes.nodes = nodes;
    }
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
enum Row {
    Group(usize),
    File(usize, usize),
}

/// A file still in the tree.
fn live(app: &App, c: &Candidate) -> Option<NodeRef> {
    app.dupes.nodes.get(&c.path).copied().flatten()
}

/// The groups with at least two files left, each followed by its files.
fn rows(app: &App) -> Vec<Row> {
    let Some(f) = found(app) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for (g, group) in f.groups.iter().enumerate() {
        let left: Vec<usize> = (0..group.files.len())
            .filter(|&i| live(app, &group.files[i]).is_some())
            .collect();
        if left.len() < 2 {
            continue;
        }
        out.push(Row::Group(g));
        out.extend(left.into_iter().map(|i| Row::File(g, i)));
    }
    out
}

fn mark_of(c: &Candidate, node: NodeRef) -> marks::Item {
    marks::Item {
        target: Target {
            node: Some(node),
            path: c.path.clone(),
            ident: Some(Ident {
                dev_ino: c.dev_ino,
                kind: Kind::File,
            }),
        },
        is_dir: false,
        size: c.freed(),
        items: 1,
        shared_out: 0,
        scanning: false,
        inside: false,
    }
}

/// The files of `group` still in the tree, but the oldest (ties: the first in path order).
fn copies(app: &App, group: &Group) -> Vec<(Candidate, NodeRef)> {
    let mut left: Vec<(Candidate, NodeRef)> = group
        .files
        .iter()
        .filter_map(|c| Some((c.clone(), live(app, c)?)))
        .collect();
    let keep = left
        .iter()
        .enumerate()
        .min_by_key(|(_, (c, _))| c.mtime)
        .map(|(i, _)| i);
    if let Some(i) = keep {
        left.remove(i);
    }
    left
}

/// Marks every copy but the oldest in each group that frees something; returns how many were
/// newly marked.
fn mark_all(app: &mut App) -> usize {
    let Some(f) = found(app) else {
        return 0;
    };
    let picks: Vec<(Candidate, NodeRef)> = f
        .groups
        .iter()
        .filter(|g| !g.all_clones())
        .flat_map(|g| copies(app, g))
        .filter(|(_, n)| !app.marks.contains(*n))
        .collect();
    for (c, n) in &picks {
        marks::set(app, mark_of(c, *n), true);
    }
    picks.len()
}

pub fn key(app: &mut App, k: &KeyEvent) -> Option<Vec<Effect>> {
    let rows = rows(app);
    let last = rows.len().saturating_sub(1);
    let page = app.list_height.max(1);
    let d = &mut app.dupes;
    match k.code {
        KeyCode::Up | KeyCode::Char('k') => d.cursor = d.cursor.saturating_sub(1),
        KeyCode::Down | KeyCode::Char('j') => d.cursor = (d.cursor + 1).min(last),
        KeyCode::PageUp => d.cursor = d.cursor.saturating_sub(page),
        KeyCode::PageDown => d.cursor = (d.cursor + page).min(last),
        KeyCode::Home | KeyCode::Char('g') => d.cursor = 0,
        KeyCode::End | KeyCode::Char('G') => d.cursor = last,
        KeyCode::Char('r') => {
            if let Some(r) = d.run.take() {
                r.cancel.store(true, Ordering::Relaxed);
            }
            return Some(start(app));
        }
        KeyCode::Char('a') => {
            let n = mark_all(app);
            app.status = Some(match n {
                0 => "every copy is marked already, or there are none to free".into(),
                1 => "marked 1 copy (the oldest file of each group is kept)".into(),
                n => format!("marked {n} copies (the oldest file of each group is kept)"),
            });
        }
        KeyCode::Char(' ') => {
            let Some(f) = found(app) else {
                return Some(Vec::new());
            };
            match rows.get(app.dupes.cursor) {
                Some(&Row::File(g, i)) => {
                    let c = f.groups[g].files[i].clone();
                    if let Some(n) = live(app, &c) {
                        let on = !app.marks.contains(n);
                        marks::set(app, mark_of(&c, n), on);
                    }
                    app.dupes.cursor = (app.dupes.cursor + 1).min(last);
                }
                Some(&Row::Group(g)) => {
                    let picks = copies(app, &f.groups[g]);
                    let on = !picks.iter().all(|(_, n)| app.marks.contains(*n));
                    for (c, n) in &picks {
                        marks::set(app, mark_of(c, *n), on);
                    }
                }
                None => {}
            }
        }
        KeyCode::Enter | KeyCode::Right | KeyCode::Char('l') => {
            let f = found(app)?;
            if let Some(&Row::File(g, i)) = rows.get(app.dupes.cursor) {
                let (path, root) = (
                    f.groups[g].files[i].path.clone(),
                    app.dupes.run.as_ref()?.root.clone(),
                );
                app.dupes.on = false;
                app.go_to(&path, &root);
            }
        }
        _ => return None,
    }
    Some(Vec::new())
}

/// `path` relative to `root`, or whole when it is not below it.
fn shown(path: &Path, root: &Path) -> String {
    match path.strip_prefix(root) {
        Ok(rel) if !rel.as_os_str().is_empty() => rel.display().to_string(),
        _ => path.display().to_string(),
    }
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
        let msg = " duplicates: not compared yet (r compares the files of the scan)";
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
        human(MIN_SIZE),
        human(wasted),
        count(found.compared)
    );
    if found.skipped > 0 {
        summary += &format!(" · {} skipped", count(found.skipped));
    }
    let mut lines = vec![Line::styled(summary, t.fg_dim)];
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
    for (i, row) in rows.iter().enumerate().skip(scroll).take(fit) {
        let mut line = match *row {
            Row::Group(g) => {
                let gr = &found.groups[g];
                let n = gr.files.iter().filter(|c| live(app, c).is_some()).count();
                let free = if gr.all_clones() {
                    "clones: deleting copies frees nothing".to_string()
                } else {
                    format!("{} to free", human(gr.wasted()))
                };
                Line::from(vec![
                    Span::styled(format!(" {n} copies of {}", human(gr.size)), t.title),
                    Span::styled(format!(" · {free}"), t.fg_dim),
                ])
            }
            Row::File(g, j) => {
                let c = &found.groups[g].files[j];
                let marked = live(app, c).is_some_and(|n| app.marks.contains(n));
                let mark = if marked { "●" } else { " " };
                let date = crate::details::date(c.mtime);
                let path = shown(&c.path, &run.root);
                let room = width.saturating_sub(16);
                let path: String = if path.chars().count() > room {
                    let skip = path.chars().count() - room.saturating_sub(1);
                    format!("…{}", path.chars().skip(skip).collect::<String>())
                } else {
                    path
                };
                Line::from(vec![
                    Span::styled(format!("   {mark} "), t.key),
                    Span::raw(path),
                    Span::styled(format!("  {date}"), t.fg_dim),
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

#[cfg(test)]
mod tests {
    use std::fs;
    use std::time::{Duration, Instant};

    use dfdu_engine::{Command as EngineCommand, Engine, EngineOptions};
    use dfdu_scan::{JobOptions, PortableBackend};
    use ratatui::crossterm::event::KeyCode;

    use super::*;
    use crate::snapshot;
    use crate::tests::{press, render};
    use crate::theme::Theme;

    #[test]
    fn duplicates_are_found_marked_and_shown() {
        let tmp = tempfile::tempdir_in(dunce::canonicalize(std::env::temp_dir()).unwrap()).unwrap();
        let dir = tmp.path().to_path_buf();
        fs::create_dir_all(dir.join("photos/old")).unwrap();
        let data: Vec<u8> = (0..(MIN_SIZE as u32 + 5000))
            .map(|i| (i % 249) as u8)
            .collect();
        fs::write(dir.join("photos/a.jpg"), &data).unwrap();
        fs::write(dir.join("photos/old/a copy.jpg"), &data).unwrap();
        fs::write(dir.join("b.jpg"), &data).unwrap();
        fs::write(dir.join("other.jpg"), vec![1u8; data.len()]).unwrap();
        let engine = Engine::start(
            PortableBackend::default(),
            EngineOptions {
                workers: 2,
                ..Default::default()
            },
        );
        engine.send(EngineCommand::Scan {
            path: dir.clone(),
            opts: JobOptions::default(),
        });
        assert!(engine.wait_idle(Some(Duration::from_secs(20))).is_idle());
        let t0 = Instant::now();
        let mut app = App::new(Theme::no_color(), t0);
        let mut target = Target::root();
        let snap = engine.read(|m| snapshot::build(m, &mut target, app.size_mode));
        app.target = target;
        app.refresh(snap, t0);

        let fx = app.key(press(KeyCode::Char('4')), t0);
        assert!(matches!(fx[..], [Effect::FindDupes]));
        let (root, cands) = engine.read(|m| {
            let r = m.tree.roots().next().unwrap();
            (
                m.tree.path_buf(r),
                dfdu_dupes::candidates(&m.tree, r, MIN_SIZE),
            )
        });
        assert_eq!(cands.len(), 4);
        begin(&mut app, root, cands);
        let deadline = Instant::now() + Duration::from_secs(20);
        while running(&app) {
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(stale(&app), "the files need their nodes");
        let nodes = engine.read(|m| read(m, &app));
        apply(&mut app, nodes);
        assert!(!stale(&app));
        engine.shutdown(Duration::from_millis(200));

        let out = render(&mut app, t0, 100, 12);
        assert!(out.contains("3 copies of 1.0 MiB"), "{out}");
        assert!(out.contains("photos/old/a copy.jpg"), "{out}");
        assert!(!out.contains("other.jpg"), "{out}");
        // `a` marks every copy but the oldest.
        app.key(press(KeyCode::Char('a')), t0);
        assert_eq!(app.marks.items().len(), 2);
        app.key(press(KeyCode::Char('a')), t0);
        assert_eq!(app.marks.items().len(), 2);
        // Space on the group unmarks its copies again.
        app.dupes.cursor = 0;
        app.key(press(KeyCode::Char(' ')), t0);
        assert!(app.marks.is_empty());
        // A deleted file leaves the group; two left still make one.
        let gone = found(&app).unwrap().groups[0].files[0].path.clone();
        app.dupes.nodes.insert(gone, None);
        let out = render(&mut app, t0, 100, 12);
        assert!(out.contains("2 copies of 1.0 MiB"), "{out}");
    }
}
