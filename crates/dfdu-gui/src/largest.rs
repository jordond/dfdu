//! The Largest view: the largest files anywhere below the current directory, as the session's
//! walk found them (`dfdu_session::largest`, which paces its walks during a scan), in a table
//! that lays out only the rows on screen: the mark, the size with its bar against the largest
//! file, the share of the directory, and the path relative to it.
//!
//! A click selects a file, a double click shows it in Files as `Enter` does, and a click on its
//! mark ticks it as `Space` does; the keys that delete act on what is ticked here.

use dfdu_engine::tilde;
use dfdu_session::SessionState;
use dfdu_session::format::{count, human, percent, ratio};
use dfdu_session::largest::Intent as Largest;
use dfdu_session::palette::Token;
use egui::{Align, Layout, Sense, Ui};
use egui_extras::{Column, TableBuilder};

use crate::browser::{Intent, ViewState};
use crate::list::{self, BAR_WIDTH};
use crate::theme::Colors;

/// The names of the table's columns, as the accessibility tree has them.
const COLUMNS: [&str; 5] = ["Marked", "Size", "Size bar", "Share", "Path"];

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
        ui.label(colors.text(format!("Reading the files below {dir}…"), Token::FgDim));
        return list::visible_rows(ui);
    }
    if l.files.is_empty() {
        ui.label(colors.text(format!("No files below {dir}."), Token::FgDim));
        return list::visible_rows(ui);
    }
    let found = format!(
        "The largest {} of {} files below {dir}",
        l.files.len(),
        count(l.count)
    );
    ui.label(colors.text(found, Token::FgDim));
    let visible = list::visible_rows(ui);
    let row_h = list::row_height(ui);
    let largest = l.files[0].size.max(1);
    let total = s.size_mode.of(&s.snap.dir_total).max(1);
    let cursor = l.cursor.min(l.files.len() - 1);
    list::whole_rows(ui);
    let mut table = TableBuilder::new(ui)
        .id_salt(("largest", &s.snap.dir_path))
        .striped(true)
        .sense(Sense::click())
        .cell_layout(Layout::left_to_right(Align::Center))
        .column(Column::exact(row_h))
        .column(Column::auto().at_least(70.0))
        .column(Column::exact(BAR_WIDTH))
        .column(Column::auto().at_least(55.0))
        .column(Column::remainder())
        .min_scrolled_height(0.0)
        .auto_shrink(false);
    if std::mem::take(&mut view.follow) {
        table = table.scroll_to_row(cursor, None);
    }
    let head = |ui: &mut Ui, text: String| {
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            ui.label(colors.text(text, Token::Header));
        });
    };
    table
        .header(row_h, |mut h| {
            h.col(|ui| list::name_column(ui, "Marked"));
            h.col(|ui| {
                list::name_column(ui, "Size");
                head(ui, "Size".into());
            });
            h.col(|ui| list::name_column(ui, "Size bar"));
            h.col(|ui| {
                list::name_column(ui, "Share");
                head(ui, "Share".into());
            });
            h.col(|ui| {
                list::name_column(ui, "Path");
                ui.label(colors.text("Path", Token::Header));
            });
        })
        .body(|body| {
            body.rows(row_h, l.files.len(), |mut row| {
                let i = row.index();
                let f = &l.files[i];
                row.set_selected(cursor == i);
                let marked = s.marks.contains(f.node);
                list::cell(&mut row, &COLUMNS, |ui| {
                    if !s.read_only && list::dot(ui, colors, marked).clicked() {
                        out.push(Intent::Largest(Largest::Select(i)));
                        out.push(Intent::Largest(Largest::Toggle));
                        out.push(Intent::Largest(Largest::Select(i)));
                    }
                });
                list::cell(&mut row, &COLUMNS, |ui| {
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        let token = if marked { Token::Mark } else { Token::Fg };
                        ui.label(colors.text(human(f.size), token));
                    });
                });
                list::cell(&mut row, &COLUMNS, |ui| {
                    list::bar(ui, colors, ratio(f.size, largest))
                });
                list::cell(&mut row, &COLUMNS, |ui| {
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        let share = format!("{:.1}%", percent(f.size, total));
                        ui.label(colors.text(share, Token::FgDim));
                    });
                });
                list::cell(&mut row, &COLUMNS, |ui| {
                    let path = f.path.display().to_string();
                    let label = colors.text(path, Token::Fg);
                    // Not selectable, so a click on the path reaches the row.
                    ui.add(egui::Label::new(label).selectable(false).truncate());
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
    visible
}
