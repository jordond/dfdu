//! The browser's windows over the list: the error list (`!`, or a click on the errors in the
//! status bar), where a click on an entry shows it in the list; and the type picker (`t`), which
//! shows only files of the types and extensions chosen, everywhere below the current directory
//! (`dfdu_session::type_filter`). `Esc` closes either. Both are on `raised` with a title in
//! `ui-title` and their rows in mono; [`field`] is the search field they, the help and the
//! palette draw as the toolbar draws its filter.

use dfdu_engine::rel_text;
use dfdu_session::SessionState;
use dfdu_session::format::{count, human};
use dfdu_session::palette::Token;
use dfdu_session::type_filter::{self, Row};
use egui::{
    Align, Align2, CornerRadius, FontFamily, FontId, Frame, Id, Label, Layout, Margin, Response,
    ScrollArea, Sense, Stroke, StrokeKind, TextEdit, Ui, UiBuilder, Window, vec2,
};

use crate::browser::{Intent, ViewState};
use crate::kit;
use crate::theme::Colors;
use crate::theme::style::{CONTROL, RADIUS_MD, ROW};

/// The ids of the windows.
const ERRORS: &str = "dfdu-errors";
const TYPES: &str = "dfdu-types";

/// The frame of a window over the browser: raised, radius 8, with 16 of room.
pub fn frame(ctx: &egui::Context) -> Frame {
    Frame::window(&ctx.global_style()).inner_margin(Margin::same(16))
}

/// A text field as the toolbar's filter draws it, `width` wide and 28 high: on `raised`,
/// outlined in `border` (2px `border.focus` while it has the keyboard), its text and the dim
/// `hint` in mono.
pub fn field(
    ui: &mut Ui,
    colors: &Colors,
    text: &mut String,
    id: Id,
    hint: &str,
    width: f32,
) -> Response {
    let focused = ui.memory(|m| m.has_focus(id));
    let (outer, _) = ui.allocate_exact_size(vec2(width, CONTROL), Sense::hover());
    let edge = if focused {
        colors.fg(Token::BorderFocus)
    } else {
        colors.fg(Token::Border)
    };
    let radius = CornerRadius::same(RADIUS_MD);
    ui.painter()
        .rect_filled(outer, radius, colors.chrome.raised);
    ui.painter().rect_stroke(
        outer,
        radius,
        Stroke::new(if focused { 2.0 } else { 1.0 }, edge),
        StrokeKind::Inside,
    );
    let mut inner = ui.new_child(
        UiBuilder::new()
            .max_rect(outer.shrink2(vec2(8.0, 0.0)))
            .layout(Layout::left_to_right(Align::Center)),
    );
    let mut edit = TextEdit::singleline(text)
        .id(id)
        .font(FontId::new(12.0, FontFamily::Monospace))
        .frame(Frame::NONE)
        .align(Align2::LEFT_CENTER)
        .min_size(vec2(0.0, CONTROL - 2.0))
        .desired_width(inner.available_width());
    if !hint.is_empty() {
        edit = edit.hint_text(kit::mono(hint, 12.0).color(colors.fg(Token::FgDim)));
    }
    inner.add(edit)
}

/// Dim copy of 12.
fn dim(colors: &Colors, text: impl Into<String>) -> egui::RichText {
    kit::sans(text, 12.0).color(colors.fg(Token::FgDim))
}

/// The error list, while it is open.
pub fn errors(ctx: &egui::Context, colors: &Colors, view: &mut ViewState, out: &mut Vec<Intent>) {
    let Some(list) = &view.errors else { return };
    let mut open = true;
    Window::new(kit::title("Errors", 15.0))
        .id(Id::unique(ERRORS))
        .frame(frame(ctx))
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
            ui.label(dim(colors, head));
            if list.items.is_empty() {
                ui.label(dim(colors, "Nothing could not be read."));
            }
            ui.add_space(4.0);
            ui.spacing_mut().item_spacing.y = 0.0;
            ScrollArea::vertical().auto_shrink(false).show_rows(
                ui,
                ROW,
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
                        let size = vec2(ui.available_width(), ROW);
                        let layout = Layout::left_to_right(Align::Center);
                        ui.allocate_ui_with_layout(size, layout, |ui| {
                            ui.spacing_mut().item_spacing.x = 12.0;
                            let path = kit::mono(path, 12.0).color(colors.fg(Token::Dir));
                            if ui.add(egui::Link::new(path)).clicked() {
                                out.push(Intent::ShowError(i));
                            }
                            let problem =
                                kit::mono(&e.problem, 12.0).color(colors.fg(Token::StatusError));
                            ui.add(Label::new(problem).truncate());
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
    Window::new(kit::title("File types", 15.0))
        .id(Id::unique(TYPES))
        .frame(frame(ctx))
        .open(&mut open)
        .default_size([360.0, 420.0])
        .collapsible(false)
        .show(ctx, |ui| {
            let width = ui.available_width();
            let id = Id::unique("dfdu-types-search");
            field(ui, colors, query, id, "Type or .extension", width);
            ui.add_space(4.0);
            if s.snap.breakdown.is_none() {
                ui.label(dim(colors, "Counting the files by type…"));
            }
            ScrollArea::vertical().auto_shrink(false).show(ui, |ui| {
                egui::Grid::new("dfdu-types")
                    .min_row_height(ROW)
                    .spacing(vec2(16.0, 0.0))
                    .show(ui, |ui| {
                        for (row, totals) in type_filter::rows(s, query) {
                            let mut chosen = type_filter::chosen(&s.types, row);
                            let name = kit::mono(label(row), 12.0);
                            if ui.checkbox(&mut chosen, name).changed() {
                                out.push(Intent::Type(row));
                            }
                            ui.label(kit::mono(human(s.size_mode.of(&totals)), 12.0));
                            let files = format!("{} files", count(totals.items));
                            ui.label(kit::mono(files, 11.0).color(colors.fg(Token::FgDim)));
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
    let bar = Id::unique(id).with("__title_click");
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
