//! The Duplicates view (`4`): files with identical contents below the scan root, as the
//! terminal's view lists them. Switching the view on compares nothing: it says how many files a
//! comparison would read, and `Compare now` (or `Enter`) starts one in a thread of its own,
//! whose progress the view shows with a button that cancels it. Then a summary bar with what
//! there is to free and the keep rule (`K` steps through them), and the groups, the most space
//! to free first, clones last and closed: each says the size of each copy, how many copies,
//! the directory its files are in and what deleting them frees, and opens to its files, each
//! with its tick (disabled and `kept` on the copy the keep rule keeps), path, size and day
//! modified. `A` ticks every copy but the one kept in each group that frees something, and `R`
//! compares again. Leaving the view stops a comparison that runs. A right click opens a row's
//! menu (`menu::review`). Below the table, the selected copy, which one is kept and which go
//! (`detail`). What is compared, listed and ticked is the session's
//! (`dfdu_session::dupes`): a click or a key here is one of its intents.

use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use dfdu_dupes::{Candidate, Found, Group, Progress};
use dfdu_session::SessionState;
use dfdu_session::actions::{Action, first_key};
use dfdu_session::dupes::{self, Intent as Ask, Keep, Row};
use dfdu_session::format::{count, human, plural, ratio};
use dfdu_session::palette::Token;
use egui::{Align, Label, Layout, Sense, Ui, vec2};
use egui_extras::{Column, TableBuilder, TableRow};

use crate::browser::{Intent, ViewState};
use crate::kit::{self, Kind};
use crate::list::{self, Ink, Point};
use crate::menu;
use crate::review::{self, ROW, Tick, right};
use crate::theme::{Colors, style};

mod detail;

/// The names of the table's columns, as the accessibility tree has them.
const COLUMNS: [&str; 4] = ["Ticked", "Path", "Size", "Modified"];

/// The height of a group's row: a header, with room above it.
const GROUP: f32 = 32.0;

/// Draws the view and returns how many rows it had room for.
pub fn show(
    ui: &mut Ui,
    s: &SessionState,
    colors: &Colors,
    view: &mut ViewState,
    out: &mut Vec<Intent>,
) -> usize {
    let visible = list::visible_rows(ui);
    let Some(run) = &s.dupes.run else {
        not_compared(ui, s, colors, out);
        return visible;
    };
    let Some(found) = run.result.get() else {
        progress(ui, &run.progress, colors, out);
        return visible;
    };
    let rows = dupes::rows(s);
    if rows.is_empty() {
        let none = format!(
            "No duplicates among the {} files compared.",
            count(found.compared)
        );
        review::empty(ui, colors, &[&what_it_does(), &none], |ui| {
            compare_again(ui, colors, out);
        });
        return visible;
    }
    summary(ui, s, colors, found, out);
    let cursor = s.dupes.cursor.min(rows.len() - 1);
    egui::Panel::bottom("dfdu-dupes-detail")
        .resizable(false)
        .frame(egui::Frame::new().inner_margin(egui::Margin::symmetric(16, 10)))
        .show(ui, |ui| {
            detail::show(ui, s, colors, found, rows.get(cursor), &run.root);
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
    let what = what_it_does();
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
            let many = format!(
                "{} files share their size with another. Nothing is compared until you ask: \
                 comparing reads the files and can take a while.",
                count(n as u64)
            );
            review::empty(ui, colors, &[&what, &many], |ui| {
                let key = first_key(Action::Enter);
                if kit::button(ui, colors, Kind::Primary, "Compare now", Some(key), true).clicked()
                {
                    out.push(Intent::Dupes(Ask::Compare));
                }
            });
            return;
        }
    };
    review::empty(ui, colors, &[&what, &why], |_| {});
}

/// A comparison running: where it is, a bar of the bytes read, and the button that stops it.
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
    let done = ratio(get(&p.bytes), get(&p.round_bytes).max(1));
    review::empty(ui, colors, &[&text], |ui| {
        let (rect, _) = ui.allocate_exact_size(vec2(320.0, 8.0), Sense::hover());
        kit::paint_bar(ui, colors, rect, done);
        ui.add_space(style::SPACE_3);
        if kit::button(ui, colors, Kind::Secondary, "Cancel", None, true).clicked() {
            out.push(Intent::DupesCancel);
        }
    });
}

fn compare_again(ui: &mut Ui, colors: &Colors, out: &mut Vec<Intent>) {
    let key = first_key(Action::CompareAgain);
    if kit::button(
        ui,
        colors,
        Kind::Secondary,
        "Compare again",
        Some(key),
        true,
    )
    .clicked()
    {
        out.push(Intent::Act(Action::CompareAgain));
    }
}

/// The summary bar: what there is to free, the groups and what is ticked, the keep rule, and
/// the buttons of the view.
fn summary(ui: &mut Ui, s: &SessionState, colors: &Colors, found: &Found, out: &mut Vec<Intent>) {
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
    let n = groups as u64;
    let mut line = format!(
        "{} group{} · {} ticked ({})",
        count(n),
        plural(n),
        count(ticked),
        human(ticked_size)
    );
    if found.skipped > 0 {
        line.push_str(&format!(" · {} skipped", count(found.skipped)));
    }
    review::summary(ui, colors, (&human(free), "to free"), &line, |ui| {
        compare_again(ui, colors, out);
        if !s.read_only {
            let key = first_key(Action::TickSuggested);
            if kit::button(
                ui,
                colors,
                Kind::Secondary,
                "Tick suggested",
                Some(key),
                true,
            )
            .clicked()
            {
                out.push(Intent::Act(Action::TickSuggested));
            }
        }
        kit::key_cap(ui, colors, first_key(Action::DupesKeep));
        let at = (Keep::ALL.iter())
            .position(|&k| k == s.dupes.keep)
            .unwrap_or(0);
        let names = Keep::ALL.map(Keep::label);
        if let Some(i) = kit::segmented(ui, colors, "Keep", &names, at) {
            // The session steps through the rules in this order, round the end.
            for _ in 0..(i + Keep::ALL.len() - at) % Keep::ALL.len() {
                out.push(Intent::Dupes(Ask::CycleKeep));
            }
        }
        ui.label(kit::label("Keep", 12.0).color(colors.fg(Token::FgDim)));
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
    let cursor = s.dupes.cursor.min(rows.len() - 1);
    let across = ui.available_rect_before_wrap().x_range();
    list::whole_rows(ui);
    let mut table = TableBuilder::new(ui)
        .id_salt("dfdu-dupes")
        .striped(false)
        .sense(Sense::click())
        .cell_layout(Layout::left_to_right(Align::Center))
        .column(Column::exact(64.0))
        .column(Column::remainder().at_least(160.0).clip(true))
        .column(Column::exact(96.0))
        .column(Column::exact(96.0))
        .min_scrolled_height(0.0)
        .auto_shrink(false);
    if std::mem::take(&mut view.follow) {
        table = table.scroll_to_row(cursor, None);
    }
    let heights = rows.iter().map(|r| match r {
        Row::Group(_) => GROUP,
        Row::File(..) => ROW,
    });
    table.body(|body| {
        body.heterogeneous_rows(heights, |mut row| {
            let i = row.index();
            row.set_selected(i == cursor);
            let ink = Ink::new(colors, i == cursor);
            let tick = match rows[i] {
                Row::Group(g) => group(
                    &mut row,
                    s,
                    (colors, &ink),
                    (g, &found.groups[g], root, across),
                    i,
                    out,
                ),
                Row::File(g, f) => file(
                    &mut row,
                    s,
                    (colors, &ink),
                    (&found.groups[g], f, root),
                    i,
                    out,
                ),
            };
            let r = row.response();
            let name = match rows[i] {
                Row::Group(g) => {
                    let group = &found.groups[g];
                    let live = (group.files.iter())
                        .filter(|c| dupes::live(&s.extras.dupes, c).is_some())
                        .count();
                    format!(
                        "{live} copies of {}, frees {}",
                        human(group.size),
                        human(group.wasted())
                    )
                }
                Row::File(g, f) => {
                    let c = &found.groups[g].files[f];
                    let path = relative_to_root(&c.path, root);
                    format!("{}, {}", path.display(), human(c.size))
                }
            };
            list::name_row(&r, i == cursor, name);
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

/// A group's row on row `i`, a header: its triangle and tick (every copy but the one kept),
/// then `1.0 MiB each × 3 copies · in photos/`, and what deleting the copies frees; clones
/// dimmed, freeing nothing. A rule in `divider` under it. Returns its tick.
fn group(
    row: &mut TableRow<'_, '_>,
    s: &SessionState,
    (colors, ink): (&Colors, &Ink),
    (g, group, root, across): (usize, &Group, &Path, egui::Rangef),
    i: usize,
    out: &mut Vec<Intent>,
) -> Tick {
    let nodes = &s.extras.dupes;
    let copies = dupes::copies(group, s.dupes.keep, nodes);
    let ticked = copies.iter().filter(|(_, n)| s.marks.contains(*n)).count();
    let tick = review::tick_of(ticked, copies.len());
    let dim = ink.of(colors.fg(Token::FgDim));
    let clones = group.all_clones();
    list::cell(row, &COLUMNS, |ui| {
        let (rect, r) = ui.allocate_exact_size(vec2(16.0, 16.0), Sense::click());
        let open = dupes::open(s, g, group);
        let token = if r.hovered() { Token::Fg } else { Token::FgDim };
        let point = if open { Point::Down } else { Point::Right };
        list::triangle(ui, ink.of(colors.fg(token)), rect, point);
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
    list::cell(row, &COLUMNS, |ui| {
        let n = (group.files.iter())
            .filter(|c| dupes::live(nodes, c).is_some())
            .count();
        let (what, token) = if clones {
            ("clones", Token::FgDim)
        } else {
            ("copies", Token::Header)
        };
        let color = ink.of(colors.fg(token));
        ui.spacing_mut().item_spacing.x = 6.0;
        ui.label(kit::mono_bold(human(group.size), 13.0).color(color));
        ui.label(kit::title(format!("each × {n} {what}"), 13.0).color(color));
        let mut rest = String::new();
        if clones {
            rest.push_str("· nothing to free (clones share their data)");
        }
        let dir = shared_dir(s, group, root);
        if !dir.as_os_str().is_empty() {
            let sep = std::path::MAIN_SEPARATOR;
            rest.push_str(&format!(" · in {}{sep}", dir.display()));
        }
        ui.add(
            Label::new(kit::sans(rest.trim_start(), 12.0).color(dim))
                .selectable(false)
                .truncate(),
        );
    });
    list::cell(row, &COLUMNS, |ui| {
        if !clones {
            right(ui, kit::mono_bold(human(group.wasted()), 12.0).color(dim));
        }
    });
    list::cell(row, &COLUMNS, |ui| {
        if !clones {
            right(ui, kit::sans("wasted", 12.0).color(dim));
        }
        // From the last cell, over the row's fill.
        review::rule_under(ui, across, colors.chrome.divider);
    });
    tick
}

/// A copy's row on row `i`: its tick, disabled on the copy the keep rule keeps, its path below
/// the scan root (`kept` after the one kept), size and day modified. Returns its tick,
/// `Locked` on the copy kept.
fn file(
    row: &mut TableRow<'_, '_>,
    s: &SessionState,
    (colors, ink): (&Colors, &Ink),
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
    let dim = ink.of(colors.fg(Token::FgDim));
    list::cell(row, &COLUMNS, |ui| {
        ui.add_space(24.0);
        if s.read_only {
            return;
        }
        if tick == Tick::Locked {
            review::locked(ui, colors, "Kept copy");
        } else if review::tick(ui, colors, tick).clicked() {
            out.extend(toggle(i));
            out.push(Intent::Dupes(Ask::Select(i)));
        }
    });
    list::cell(row, &COLUMNS, |ui| {
        let path = relative_to_root(&c.path, root).display().to_string();
        let token = match () {
            () if ticked => Token::Mark,
            () if group.all_clones() => Token::FgDim,
            () => Token::Fg,
        };
        let path = Label::new(kit::mono(path, 13.0).color(ink.of(colors.fg(token))));
        let path = path.selectable(false).truncate();
        if tick == Tick::Locked {
            // Room for the word after the path.
            ui.scope(|ui| {
                ui.set_max_width(ui.available_width() - 48.0);
                ui.add(path);
            });
            ui.label(kit::sans("kept", 12.0).color(dim));
        } else {
            ui.add(path);
        }
    });
    list::cell(row, &COLUMNS, |ui| {
        let token = if ticked { Token::Mark } else { Token::SizeBig };
        let size = kit::mono_bold(human(c.size), 13.0).color(ink.of(colors.fg(token)));
        right(ui, size);
    });
    list::cell(row, &COLUMNS, |ui| {
        right(ui, kit::sans(dfdu_tree::day(c.mtime), 12.0).color(dim));
    });
    tick
}
