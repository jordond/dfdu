//! The Duplicates view (`4`): files with identical contents below the scan root, as the
//! terminal's view lists them. Switching the view on compares nothing: it says how many files a
//! comparison would read, and `Compare now` (or `Enter`) starts one in a thread of its own,
//! whose progress the view shows with a button that cancels it. Then the groups, the most space
//! to free first, clones last and closed: each says how many copies, what deleting them frees
//! and the directory its files are in, and opens to its files, each with its tick (or `kept` on
//! the copy the keep rule keeps), size, day modified and path. The keep rule is picked above
//! the table (`K` steps through them); `A` ticks every copy but the one kept in each group that
//! frees something, and `R` compares again. Leaving the view stops a comparison that runs. A
//! right click opens a row's menu (`menu::review`).
//! Below the table, the selected copy, which one is kept and which go. What is compared,
//! listed and ticked is the session's (`dfdu_session::dupes`): a click or a key here is one of
//! its intents.

use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use dfdu_dupes::{Candidate, Found, Group, Progress};
use dfdu_session::SessionState;
use dfdu_session::actions::{Action, first_key};
use dfdu_session::dupes::{self, Intent as Ask, Keep, Row};
use dfdu_session::format::{count, human, plural, ratio};
use dfdu_session::palette::Token;
use dfdu_tree::HostPath;
use egui::{Align, Label, Layout, ProgressBar, RichText, Sense, Ui, vec2};
use egui_extras::{Column, TableBuilder, TableRow};

use crate::browser::{Intent, ViewState};
use crate::list::{self, Point};
use crate::menu;
use crate::review::{self, Tick};
use crate::theme::Colors;

/// Copies of a group listed below the table before "and N more".
const SHOWN: usize = 6;

/// Draws the view and returns how many rows it had room for.
pub fn show(
    ui: &mut Ui,
    s: &SessionState,
    colors: &Colors,
    view: &mut ViewState,
    out: &mut Vec<Intent>,
) -> usize {
    let dim = |t: String| colors.text(t, Token::FgDim);
    let Some(run) = &s.dupes.run else {
        not_compared(ui, s, colors, out);
        return list::visible_rows(ui);
    };
    let Some(found) = run.result.get() else {
        progress(ui, &run.progress, colors, out);
        return list::visible_rows(ui);
    };
    let rows = dupes::rows(s);
    if rows.is_empty() {
        ui.label(dim(what_it_does()));
        ui.label(dim(format!(
            "No duplicates among the {} files compared.",
            count(found.compared)
        )));
        compare_again(ui, out);
        return list::visible_rows(ui);
    }
    controls(ui, s, colors, found, out);
    let cursor = s.dupes.cursor.min(rows.len() - 1);
    egui::Panel::bottom("dfdu-dupes-detail")
        .resizable(false)
        .show(ui, |ui| {
            detail(ui, s, colors, found, rows.get(cursor), &run.root)
        });
    let visible = list::visible_rows(ui);
    table(ui, s, colors, view, (found, &run.root), &rows, out);
    visible
}

/// What the view does, the first line of what it says before there are groups.
fn what_it_does() -> String {
    format!(
        "Finds files with identical contents anywhere in the scan (files of {} or more).",
        human(dupes::min_size())
    )
}

/// Before anything was compared: how many files a comparison would read, and the button that
/// starts it.
fn not_compared(ui: &mut Ui, s: &SessionState, colors: &Colors, out: &mut Vec<Intent>) {
    let dim = |t: String| colors.text(t, Token::FgDim);
    ui.label(dim(what_it_does()));
    let why = match (s.snap.busy, s.dupes.candidates) {
        _ if !s.outside.local => "It reads this machine's files, and this scan is remote or from \
                                  a file: there is nothing here to compare."
            .to_owned(),
        (true, _) => "The scan is still running: compare once it is done.".to_owned(),
        (false, None) => "Counting the files to compare…".to_owned(),
        (false, Some(0)) => {
            "No two files that large have the same size: there is nothing to compare.".into()
        }
        (false, Some(n)) => {
            ui.label(dim(format!(
                "{} files share their size with another. Nothing is compared until you ask: \
                 comparing reads the files and can take a while.",
                count(n as u64)
            )));
            if ui
                .button("Compare now")
                .on_hover_text(format!("key {}", first_key(Action::Enter)))
                .clicked()
            {
                out.push(Intent::Dupes(Ask::Compare));
            }
            return;
        }
    };
    ui.label(dim(why));
}

/// A comparison running: where it is, and the button that stops it.
fn progress(ui: &mut Ui, p: &Progress, colors: &Colors, out: &mut Vec<Intent>) {
    let get = |a: &AtomicU64| a.load(Ordering::Relaxed);
    let what = match get(&p.round) {
        0 | 1 => "Comparing the starts and ends of files",
        _ => "Comparing the contents of files",
    };
    let text = format!(
        "{what}: {} of {} files, {} of {}",
        count(get(&p.files)),
        count(get(&p.round_files)),
        human(get(&p.bytes)),
        human(get(&p.round_bytes)),
    );
    ui.label(colors.text(text, Token::FgDim));
    ui.horizontal(|ui| {
        #[expect(clippy::cast_possible_truncation, reason = "a fraction drawn as a bar")]
        let done = ratio(get(&p.bytes), get(&p.round_bytes).max(1)) as f32;
        ui.add(
            ProgressBar::new(done)
                .desired_width(240.0)
                .fill(colors.fg(Token::BarFill)),
        );
        if ui.button("Cancel").clicked() {
            out.push(Intent::DupesCancel);
        }
    });
}

fn compare_again(ui: &mut Ui, out: &mut Vec<Intent>) {
    if ui
        .button("Compare again")
        .on_hover_text(format!("key {}", first_key(Action::CompareAgain)))
        .clicked()
    {
        out.push(Intent::Act(Action::CompareAgain));
    }
}

/// What the ticks add up to, the keep rule, and the buttons of the view.
fn controls(ui: &mut Ui, s: &SessionState, colors: &Colors, found: &Found, out: &mut Vec<Intent>) {
    let nodes = &s.extras.dupes;
    let (mut groups, mut ticked, mut ticked_size, mut free) = (0, 0, 0, 0);
    for g in &found.groups {
        if g.files
            .iter()
            .filter(|c| dupes::live(nodes, c).is_some())
            .count()
            < 2
        {
            continue;
        }
        groups += 1;
        for c in &g.files {
            if dupes::live(nodes, c).is_some_and(|n| s.marks.contains(n)) {
                ticked += 1;
                ticked_size += c.freed();
            }
        }
        if !g.all_clones() {
            free += g.wasted();
        }
    }
    ui.horizontal(|ui| {
        let n = groups as u64;
        let mut head = format!("{} group{} · {} to free", count(n), plural(n), human(free));
        if found.skipped > 0 {
            head.push_str(&format!(" · {} skipped", count(found.skipped)));
        }
        ui.label(colors.text(head, Token::Header));
        let token = if ticked == 0 {
            Token::FgDim
        } else {
            Token::Mark
        };
        let t = format!("{} ticked ({})", count(ticked), human(ticked_size));
        ui.label(colors.text(t, token));
        ui.separator();
        ui.label(colors.text("Keep", Token::FgDim));
        let at = Keep::ALL
            .iter()
            .position(|&k| k == s.dupes.keep)
            .unwrap_or(0);
        for (i, keep) in Keep::ALL.into_iter().enumerate() {
            let r = ui.selectable_label(i == at, keep.label());
            if r.on_hover_text(format!("key {}", first_key(Action::DupesKeep)))
                .clicked()
                && i != at
            {
                // The session steps through the rules in this order, round the end.
                for _ in 0..(i + Keep::ALL.len() - at) % Keep::ALL.len() {
                    out.push(Intent::Dupes(Ask::CycleKeep));
                }
            }
        }
        ui.separator();
        if !s.read_only
            && ui
                .button("Tick suggested")
                .on_hover_text(format!("key {}", first_key(Action::TickSuggested)))
                .clicked()
        {
            out.push(Intent::Act(Action::TickSuggested));
        }
        compare_again(ui, out);
    });
}

/// `path` relative to `root`, or whole when it is not below it.
pub fn relative_to_root(path: &Path, root: &Path) -> PathBuf {
    match path.strip_prefix(root) {
        Ok(rel) if !rel.as_os_str().is_empty() => rel.to_path_buf(),
        _ => path.to_path_buf(),
    }
}

/// The directory all of the files of `group` still in the tree are in, below `root`; empty
/// when they share none.
fn shared_dir(s: &SessionState, group: &Group, root: &Path) -> PathBuf {
    let dir = |c: &Candidate| -> Vec<PathBuf> {
        let rel = relative_to_root(&c.path, root);
        (rel.parent().map(Path::components).into_iter().flatten())
            .map(|c: Component<'_>| PathBuf::from(c.as_os_str()))
            .collect()
    };
    let mut live = (group.files.iter()).filter(|c| dupes::live(&s.extras.dupes, c).is_some());
    let Some(first) = live.next() else {
        return PathBuf::new();
    };
    let mut shared = dir(first);
    for c in live {
        let n = (shared.iter().zip(dir(c)))
            .take_while(|(a, b)| **a == *b)
            .count();
        shared.truncate(n);
    }
    shared.iter().collect()
}

/// The groups and their files, in a table that lays out only the rows on screen.
fn table(
    ui: &mut Ui,
    s: &SessionState,
    colors: &Colors,
    view: &mut ViewState,
    (found, root): (&Found, &Path),
    rows: &[Row],
    out: &mut Vec<Intent>,
) {
    let row_h = list::row_height(ui);
    let cursor = s.dupes.cursor.min(rows.len() - 1);
    list::whole_rows(ui);
    let mut table = TableBuilder::new(ui)
        .id_salt("dfdu-dupes")
        .striped(true)
        .sense(Sense::click())
        .cell_layout(Layout::left_to_right(Align::Center))
        .column(Column::exact(row_h * 3.0))
        .column(Column::auto().at_least(70.0))
        .column(Column::auto().at_least(80.0))
        .column(Column::remainder())
        .min_scrolled_height(0.0)
        .auto_shrink(false);
    if std::mem::take(&mut view.follow) {
        table = table.scroll_to_row(cursor, None);
    }
    table
        .header(row_h, |mut h| {
            for label in ["", "Size", "Modified", "Path"] {
                h.col(|ui| {
                    ui.label(colors.text(label, Token::Header));
                });
            }
        })
        .body(|body| {
            body.rows(row_h, rows.len(), |mut row| {
                let i = row.index();
                row.set_selected(i == cursor);
                let tick = match rows[i] {
                    Row::Group(g) => {
                        group(&mut row, s, colors, (g, &found.groups[g], root), i, out)
                    }
                    Row::File(g, f) => {
                        file(&mut row, s, colors, (&found.groups[g], f, root), i, out)
                    }
                };
                let r = row.response();
                if r.double_clicked() {
                    out.push(Intent::Dupes(Ask::Select(i)));
                    out.push(Intent::Dupes(Ask::Activate));
                } else if r.clicked() || r.secondary_clicked() {
                    out.push(Intent::Dupes(Ask::Select(i)));
                }
                // A copy's tick keeps the cursor on it, as its tick does.
                let path = matches!(rows[i], Row::File(..));
                let mut asks = toggle(i).to_vec();
                if path {
                    asks.push(Intent::Dupes(Ask::Select(i)));
                }
                r.context_menu(|ui| menu::review(ui, s, path, (tick, &asks), out));
            });
        });
}

/// What ticking row `i` asks, as its tick does.
fn toggle(i: usize) -> [Intent; 2] {
    [Intent::Dupes(Ask::Select(i)), Intent::Dupes(Ask::Toggle)]
}

/// A group's row on row `i`: its triangle and tick (every copy but the one kept), then
/// `3 copies of 1.0 MiB · frees 2.0 MiB · in photos/`; clones dimmed. Returns its tick.
fn group(
    row: &mut TableRow<'_, '_>,
    s: &SessionState,
    colors: &Colors,
    (g, group, root): (usize, &Group, &Path),
    i: usize,
    out: &mut Vec<Intent>,
) -> Tick {
    let nodes = &s.extras.dupes;
    let copies = dupes::copies(group, s.dupes.keep, nodes);
    let ticked = copies.iter().filter(|(_, n)| s.marks.contains(*n)).count();
    let tick = review::tick_of(ticked, copies.len());
    row.col(|ui| {
        let h = ui.text_style_height(&egui::TextStyle::Body);
        let (rect, r) = ui.allocate_exact_size(vec2(h, h), Sense::click());
        let open = dupes::open(s, g, group);
        let token = if r.hovered() { Token::Fg } else { Token::FgDim };
        let point = if open { Point::Down } else { Point::Right };
        list::triangle(ui, colors.fg(token), rect, point);
        let what = if open { "Collapse" } else { "Expand" };
        r.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, true, what));
        if r.clicked() {
            out.push(Intent::Dupes(Ask::Select(i)));
            out.push(Intent::Dupes(Ask::Activate));
        }
        if !s.read_only && review::tick(ui, colors, tick).clicked() {
            out.extend(toggle(i));
        }
    });
    row.col(|ui| right(ui, colors.text(human(group.size), Token::Fg)));
    row.col(|_| {});
    row.col(|ui| {
        let n = group
            .files
            .iter()
            .filter(|c| dupes::live(nodes, c).is_some())
            .count();
        let clones = group.all_clones();
        let (what, free, token) = if clones {
            (
                "clones",
                "frees nothing (clones share their data)".to_owned(),
                Token::FgDim,
            )
        } else {
            (
                "copies",
                format!("frees {}", human(group.wasted())),
                Token::Title,
            )
        };
        ui.label(colors.text(format!("{n} {what} of {}", human(group.size)), token));
        let dir = shared_dir(s, group, root);
        let mut rest = format!("· {free}");
        if !dir.as_os_str().is_empty() {
            rest.push_str(&format!(
                " · in {}{}",
                dir.display(),
                std::path::MAIN_SEPARATOR
            ));
        }
        ui.add(
            Label::new(colors.text(rest, Token::FgDim))
                .selectable(false)
                .truncate(),
        );
    });
    tick
}

/// A copy's row on row `i`: its tick, or `kept` on the copy the keep rule keeps, its size, day
/// modified and path below the scan root. Returns its tick, `Locked` on the copy kept.
fn file(
    row: &mut TableRow<'_, '_>,
    s: &SessionState,
    colors: &Colors,
    (group, f, root): (&Group, usize, &Path),
    i: usize,
    out: &mut Vec<Intent>,
) -> Tick {
    let nodes = &s.extras.dupes;
    let c = &group.files[f];
    let ticked = dupes::live(nodes, c).is_some_and(|n| s.marks.contains(n));
    let kept = dupes::kept(group, s.dupes.keep, nodes) == Some(f);
    let tick = match (kept, ticked) {
        (true, false) => Tick::Locked,
        (_, true) => Tick::On,
        (false, false) => Tick::Off,
    };
    row.col(|ui| {
        let h = ui.text_style_height(&egui::TextStyle::Body);
        ui.add_space(1.4 * h);
        if tick == Tick::Locked {
            ui.label(colors.text("kept", Token::StatusOk));
        } else if !s.read_only && review::tick(ui, colors, tick).clicked() {
            out.extend(toggle(i));
            out.push(Intent::Dupes(Ask::Select(i)));
        }
    });
    let token = if ticked { Token::Mark } else { Token::Fg };
    row.col(|ui| right(ui, colors.text(human(c.size), token)));
    row.col(|ui| {
        ui.label(colors.text(dfdu_tree::day(c.mtime), Token::FgDim));
    });
    row.col(|ui| {
        let path = relative_to_root(&c.path, root).display().to_string();
        let token = if group.all_clones() {
            Token::FgDim
        } else {
            Token::Fg
        };
        ui.add(
            Label::new(colors.text(path, token))
                .selectable(false)
                .truncate(),
        );
    });
    tick
}

/// `text` aligned to the right of its cell.
fn right(ui: &mut Ui, text: RichText) {
    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
        ui.label(text);
    });
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

/// Below the table: the selected copy and what deleting it frees, then which copy of its group
/// is kept and which go.
fn detail(
    ui: &mut Ui,
    s: &SessionState,
    colors: &Colors,
    found: &Found,
    row: Option<&Row>,
    root: &Path,
) {
    let Some(&row) = row else { return };
    let (Row::Group(g) | Row::File(g, _)) = row;
    let group = &found.groups[g];
    let nodes = &s.extras.dupes;
    if let Row::File(_, i) = row {
        let c = &group.files[i];
        let name = dfdu_engine::tilde(&HostPath::local(&c.path), s.home.as_deref());
        ui.label(colors.text(name, Token::Title));
        let modified = dfdu_tree::date_time(c.mtime);
        let line = format!("{} · modified {modified} · {}", human(c.size), frees(c));
        ui.label(colors.text(line, Token::FgDim));
    }
    let keep = dupes::kept(group, s.dupes.keep, nodes);
    let live: Vec<(usize, &Candidate)> = (group.files.iter().enumerate())
        .filter(|(_, c)| dupes::live(nodes, c).is_some())
        .collect();
    for &(i, c) in live.iter().take(SHOWN) {
        let rel = relative_to_root(&c.path, root).display().to_string();
        ui.horizontal(|ui| {
            if Some(i) == keep {
                ui.label(colors.text("kept", Token::StatusOk));
            } else {
                ui.label(colors.text("goes", Token::FgDim));
            }
            ui.add(Label::new(rel).truncate());
        });
    }
    if live.len() > SHOWN {
        let more = format!("and {} more", live.len() - SHOWN);
        ui.label(colors.text(more, Token::FgDim));
    }
}
