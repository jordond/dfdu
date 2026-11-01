//! The Largest view: the largest files anywhere below the current directory, as the session's
//! walk found them (`dfdu_session::largest`, which paces its walks during a scan). A summary bar
//! says how many of how many, then a table like the list's lays out only the rows on screen:
//! the mark, the name with the directory it is in (relative to the current one), the size, and
//! a bar against the largest file with the share of the directory.
//!
//! A click selects a file, a double click shows it in Files as `Enter` does, and a click on its
//! mark ticks it as `Space` does; the keys that delete act on what is ticked here.

use dfdu_engine::tilde;
use dfdu_session::SessionState;
use dfdu_session::actions::{Action, first_key};
use dfdu_session::format::{count, human, percent, ratio};
use dfdu_session::largest::{File, Intent as Largest};
use dfdu_session::palette::Token;
use egui::{Align, Label, Layout, Sense, Ui};
use egui_extras::{Column, TableBuilder};

use crate::browser::{Intent, ViewState};
use crate::kit;
use crate::list::{self, BAR_WIDTH, Ink};
use crate::review::{self, right};
use crate::theme::{Colors, style};

/// The names of the table's columns, as the accessibility tree has them.
const COLUMNS: [&str; 4] = ["Marked", "Name", "Size", "Share"];
/// The largest sizes stand out, as the list's do: the first this many rows.
const BIG: usize = 3;

/// Draws the view and returns how many rows it had room for.
pub fn show(
    ui: &mut Ui,
    s: &SessionState,
    colors: &Colors,
    view: &mut ViewState,
    out: &mut Vec<Intent>,
) -> usize {
    let l = &s.largest;
    let dir = tilde(&s.snap.dir_path, s.home.as_deref());
    if l.walked.is_none() {
        let visible = list::visible_rows(ui);
        list::empty(ui, colors, &format!("Reading the files below {dir}…"));
        return visible;
    }
    if l.files.is_empty() {
        let visible = list::visible_rows(ui);
        list::empty(ui, colors, &format!("No files below {dir}."));
        return visible;
    }
    let found = format!(
        "The largest {} of {} files below {dir}",
        l.files.len(),
        count(l.count)
    );
    let listed: u64 = l.files.iter().map(|f| f.size).sum();
    let n = l.files.len() as u64;
    let word = format!("in {} file{}", count(n), if n == 1 { "" } else { "s" });
    review::summary(ui, colors, (&human(listed), &word), &found, |ui| {
        // Right to left, so each hint's words before its key cap: `[Space] tick`.
        let dim = colors.fg(Token::FgDim);
        ui.spacing_mut().item_spacing.x = style::SPACE_1;
        for (key, what) in [
            (Action::Enter, "show in Files"),
            (Action::ToggleMark, "tick"),
        ] {
            ui.label(kit::sans(what, 12.0).color(dim));
            kit::key_cap(ui, colors, first_key(key));
            ui.add_space(style::SPACE_2);
        }
    });
    let visible = list::visible_rows(ui);
    table(ui, s, colors, view, out);
    visible
}

/// The files, in a table that lays out only the rows on screen.
fn table(
    ui: &mut Ui,
    s: &SessionState,
    colors: &Colors,
    view: &mut ViewState,
    out: &mut Vec<Intent>,
) {
    let l = &s.largest;
    let row_h = list::row_height(ui);
    let largest = l.files[0].size.max(1);
    let total = s.size_mode.of(&s.snap.dir_total).max(1);
    let cursor = l.cursor.min(l.files.len() - 1);
    list::whole_rows(ui);
    list::header_rule(ui, colors, row_h);
    let mut table = TableBuilder::new(ui)
        .id_salt(("largest", &s.snap.dir_path))
        .striped(false)
        .sense(Sense::click())
        .cell_layout(Layout::left_to_right(Align::Center))
        .column(Column::exact(22.0))
        .column(Column::remainder().at_least(160.0).clip(true))
        .column(Column::exact(96.0))
        .column(Column::exact(BAR_WIDTH))
        .min_scrolled_height(0.0)
        .auto_shrink(false);
    if std::mem::take(&mut view.follow) {
        table = table.scroll_to_row(cursor, None);
    }
    let dim = colors.fg(Token::FgDim);
    table
        .header(row_h, |mut h| {
            h.col(|ui| list::name_column(ui, "Marked"));
            h.col(|ui| {
                list::name_column(ui, "Name");
                ui.label(kit::label("Name", 12.0).color(dim));
            });
            h.col(|ui| {
                list::name_column(ui, "Size");
                // The files are by size, the largest first.
                let sorted = kit::title("Size ↓", 12.0).color(colors.fg(Token::Header));
                right(ui, sorted);
            });
            h.col(|ui| {
                list::name_column(ui, "Share");
                ui.label(kit::label("Share", 12.0).color(dim));
            });
        })
        .body(|body| {
            body.rows(row_h, l.files.len(), |mut row| {
                let i = row.index();
                let f = &l.files[i];
                row.set_selected(cursor == i);
                let ink = Ink::new(colors, cursor == i);
                let marked = s.marks.contains(f.node);
                list::cell(&mut row, &COLUMNS, |ui| {
                    if !s.read_only && list::dot(ui, colors, marked).clicked() {
                        out.push(Intent::Largest(Largest::Select(i)));
                        out.push(Intent::Largest(Largest::Toggle));
                        out.push(Intent::Largest(Largest::Select(i)));
                    }
                });
                list::cell(&mut row, &COLUMNS, |ui| name(ui, colors, &ink, f, marked));
                list::cell(&mut row, &COLUMNS, |ui| {
                    let text = human(f.size);
                    let t = if marked {
                        kit::mono_bold(text, 13.0).color(ink.of(colors.fg(Token::Mark)))
                    } else if i < BIG || ink.on {
                        kit::mono_bold(text, 13.0).color(ink.of(colors.fg(Token::SizeBig)))
                    } else {
                        kit::mono(text, 13.0).color(colors.fg(Token::Fg))
                    };
                    right(ui, t);
                });
                list::cell(&mut row, &COLUMNS, |ui| {
                    share(
                        ui,
                        colors,
                        &ink,
                        ratio(f.size, largest),
                        percent(f.size, total),
                    );
                });
                let r = row.response();
                let name = format!("{}, {}", f.path.display(), human(f.size));
                list::name_row(&r, cursor == i, name);
                if r.double_clicked() {
                    out.push(Intent::Largest(Largest::Select(i)));
                    out.push(Intent::Largest(Largest::Activate));
                } else if r.clicked() {
                    out.push(Intent::Largest(Largest::Select(i)));
                }
            });
        });
}

/// The name cell: the file's name in mono, marked in `mark`, then the directory it is in,
/// dim and smaller.
fn name(ui: &mut Ui, colors: &Colors, ink: &Ink, f: &File, marked: bool) {
    let file = f.path.file_name().map_or_else(
        || f.path.display().into_owned(),
        |n| String::from_utf8_lossy(n).into_owned(),
    );
    let token = if marked { Token::Mark } else { Token::Fg };
    let parent = (f.path.parent())
        .map(|p| p.display().into_owned())
        .filter(|p| !p.is_empty());
    ui.spacing_mut().item_spacing.x = style::SPACE_2;
    // Not selectable, so a click on the name reaches the row.
    let name = Label::new(kit::mono(file, 13.0).color(ink.of(colors.fg(token))));
    ui.add(name.selectable(false).truncate());
    if let Some(parent) = parent {
        let parent = kit::mono(parent, 11.0).color(ink.of(colors.fg(Token::FgDim)));
        ui.add(Label::new(parent).selectable(false).truncate());
    }
}

/// The share cell: a bar of `frac` of the largest file, then `pct`, the share of the directory.
fn share(ui: &mut Ui, colors: &Colors, ink: &Ink, frac: f64, pct: f64) {
    list::bar(ui, colors, frac);
    let pct = format!("{pct:.1}%");
    ui.label(kit::mono(pct, 11.0).color(ink.of(colors.fg(Token::FgDim))));
}
