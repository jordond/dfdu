//! The browser's windows over the list: the error list (`!`, or a click on the errors in the
//! status bar), where a click on an entry shows it in the list; and the type picker (`t`), which
//! shows only files of the types and extensions chosen, everywhere below the current directory
//! (`dfdu_session::type_filter`). `Esc` closes either.

use dfdu_engine::rel_text;
use dfdu_session::SessionState;
use dfdu_session::format::{count, human};
use dfdu_session::palette::Token;
use dfdu_session::type_filter::{self, Row};
use egui::{Label, ScrollArea, TextEdit, Window};

use crate::browser::{Intent, ViewState};
use crate::theme::Colors;

/// The ids of the windows.
const ERRORS: &str = "dfdu-errors";
const TYPES: &str = "dfdu-types";

/// The error list, while it is open.
pub fn errors(ctx: &egui::Context, colors: &Colors, view: &mut ViewState, out: &mut Vec<Intent>) {
    let Some(list) = &view.errors else { return };
    let mut open = true;
    Window::new("Errors")
        .id(egui::Id::unique(ERRORS))
        .open(&mut open)
        .default_size([520.0, 360.0])
        .collapsible(false)
        .show(ctx, |ui| {
            let summary = list.summary();
            let head = if list.total > list.items.len() as u64 {
                format!(
                    "{} errors, the first {} listed: {summary}",
                    count(list.total),
                    count(list.items.len() as u64)
                )
            } else {
                format!("{} errors: {summary}", count(list.total))
            };
            ui.label(colors.text(head, Token::Header));
            if list.items.is_empty() {
                ui.label(colors.text("Nothing could not be read.", Token::FgDim));
            }
            ScrollArea::vertical().auto_shrink(false).show_rows(
                ui,
                ui.text_style_height(&egui::TextStyle::Body),
                list.items.len(),
                |ui, range| {
                    for i in range {
                        let e = &list.items[i];
                        let mut path = match rel_text(&e.path, &list.root) {
                            Some(rel) if rel.is_empty() => ".".to_string(),
                            Some(rel) => rel,
                            None => e.path.to_string(),
                        };
                        if e.is_dir {
                            path.push('/');
                        }
                        ui.horizontal(|ui| {
                            if ui.link(colors.text(path, Token::Dir)).clicked() {
                                out.push(Intent::ShowError(i));
                            }
                            ui.add(
                                Label::new(colors.text(&e.problem, Token::StatusError)).truncate(),
                            );
                        });
                    }
                },
            );
        });
    name_title_bar(ctx, ERRORS, "Errors");
    if !open {
        view.errors = None;
    }
}

/// The type picker, while it is open: a search, then every file, the types and the
/// extensions below the directory, largest first, each chosen or not.
pub fn types(
    ctx: &egui::Context,
    s: &SessionState,
    colors: &Colors,
    view: &mut ViewState,
    out: &mut Vec<Intent>,
) {
    let Some(query) = &mut view.types else { return };
    let mut open = true;
    Window::new("File types")
        .id(egui::Id::unique(TYPES))
        .open(&mut open)
        .default_size([360.0, 420.0])
        .collapsible(false)
        .show(ctx, |ui| {
            ui.add(TextEdit::singleline(query).hint_text("Type or .extension"));
            if s.snap.breakdown.is_none() {
                ui.label(colors.text("Counting the files by type…", Token::FgDim));
            }
            ScrollArea::vertical().auto_shrink(false).show(ui, |ui| {
                egui::Grid::new("dfdu-types").show(ui, |ui| {
                    for (row, totals) in type_filter::rows(s, query) {
                        let mut chosen = type_filter::chosen(&s.types, row);
                        if ui.checkbox(&mut chosen, label(row)).changed() {
                            out.push(Intent::Type(row));
                        }
                        ui.label(human(s.size_mode.of(&totals)));
                        ui.label(
                            colors.text(format!("{} files", count(totals.items)), Token::FgDim),
                        );
                        ui.end_row();
                    }
                });
            });
        });
    name_title_bar(ctx, TYPES, "File types");
    if !open {
        view.types = None;
    }
}

/// Names the title bar of the window whose id is `Id::unique(id)` with its title `title` in the
/// accessibility tree: egui leaves it unnamed, though it takes clicks (a double click, a drag).
pub fn name_title_bar(ctx: &egui::Context, id: &str, title: &str) {
    let bar = egui::Id::unique(id).with("__title_click");
    ctx.accesskit_node_builder(bar, |node| node.set_label(title));
}

/// What a row of the picker is called.
fn label(r: Row) -> String {
    match r {
        Row::All => "All files".into(),
        Row::Type(t) => t.name().into(),
        Row::Ext(e) => format!(".{e}"),
    }
}
