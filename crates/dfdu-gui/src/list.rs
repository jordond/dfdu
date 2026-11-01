//! The list of the current directory: a table that lays out only the rows on screen, with the
//! mark, the size and its bar, the items, the day modified and the name, under headers that
//! sort by their column. In tree mode the names are indented by depth, and a directory's
//! triangle expands or collapses it. The rows that arrived since the last sort are one summary
//! row at the end.
//!
//! A click selects a row and a double click opens it, as `Enter` does; a click on the mark
//! column marks it, and a right click opens its menu (`menu`). Each scanned directory keeps its
//! own scroll position.

use dfdu_session::actions::{Action, first_key};
use dfdu_session::format::{count, human, plural, ratio};
use dfdu_session::order::SortKey;
use dfdu_session::palette::Token;
use dfdu_session::snapshot::{EntryData, Mark};
use dfdu_session::{SessionState, marks};
use egui::{Align, Layout, Rect, Sense, Shape, Stroke, Ui, pos2, vec2};
use egui_extras::{Column, TableBuilder, TableRow};

use crate::browser::{Intent, ViewState};
use crate::menu;
use crate::theme::Colors;

/// Width of the size bar.
const BAR_WIDTH: f32 = 80.0;
/// Indent of each tree level, in units of the row height.
const INDENT: f32 = 0.9;

/// Draws the list and returns how many rows it had room for.
pub fn show(
    ui: &mut Ui,
    s: &SessionState,
    colors: &Colors,
    view: &mut ViewState,
    out: &mut Vec<Intent>,
) -> usize {
    let row_h = ui.text_style_height(&egui::TextStyle::Body) + 4.0;
    let room = ui.available_height() - row_h - ui.spacing().item_spacing.y;
    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "a count of rows on screen"
    )]
    let visible = (room / (row_h + ui.spacing().item_spacing.y))
        .floor()
        .max(0.0) as usize;
    if s.list.rows.is_empty() && s.list.new_count == 0 {
        ui.label(colors.text(empty_note(s), Token::FgDim));
        return visible;
    }
    let selected = s.selected_index();
    let mut table = TableBuilder::new(ui)
        .id_salt(("list", &s.snap.dir_path))
        .striped(true)
        .sense(Sense::click())
        .cell_layout(Layout::left_to_right(Align::Center))
        .column(Column::exact(row_h))
        .column(Column::auto().at_least(70.0))
        .column(Column::exact(BAR_WIDTH))
        .column(Column::auto().at_least(50.0))
        .column(Column::auto().at_least(80.0))
        .column(Column::remainder())
        .min_scrolled_height(0.0)
        .auto_shrink(false);
    if std::mem::take(&mut view.follow)
        && let Some(i) = selected
    {
        table = table.scroll_to_row(i, None);
    }
    let new_row = usize::from(s.list.new_count > 0);
    table
        .header(row_h, |mut h| {
            for (key, label) in [
                (None, ""),
                (Some(SortKey::Size), "Size"),
                (None, ""),
                (Some(SortKey::Items), "Items"),
                (Some(SortKey::Mtime), "Modified"),
                (Some(SortKey::Name), "Name"),
            ] {
                h.col(|ui| header(ui, s, colors, key, label, out));
            }
        })
        .body(|body| {
            body.rows(row_h, s.list.rows.len() + new_row, |mut row| {
                let i = row.index();
                if i == s.list.rows.len() {
                    new_entries(&mut row, s, colors, out);
                    return;
                }
                row.set_selected(selected == Some(i));
                let e = &s.snap.entries[s.list.rows[i]];
                entry(&mut row, s, colors, e, i, out);
                let r = row.response();
                if r.double_clicked() {
                    out.push(Intent::Open(i));
                } else if r.clicked() || r.secondary_clicked() {
                    out.push(Intent::Select(i));
                }
                r.context_menu(|ui| menu::row(ui, s, e, i, out));
            });
        });
    visible
}

/// What an empty list says.
fn empty_note(s: &SessionState) -> String {
    match s.snap.dir_mark {
        Some(Mark::Scanning | Mark::Rescanning | Mark::Stalled) => "Scanning…".into(),
        Some(Mark::Error) => "This directory could not be read.".into(),
        Some(Mark::Incomplete) => format!("Incomplete: rescan with {}.", first_key(Action::Rescan)),
        _ if s.filter.active() => "Nothing matches the filter.".into(),
        _ => "Empty.".into(),
    }
}

/// A column's header: a click sorts by it, again reverses.
fn header(
    ui: &mut Ui,
    s: &SessionState,
    colors: &Colors,
    key: Option<SortKey>,
    label: &str,
    out: &mut Vec<Intent>,
) {
    let Some(key) = key else { return };
    let r = ui.add(egui::Button::new(colors.text(label, Token::Header)).frame(false));
    if s.sort.key == key {
        let h = ui.text_style_height(&egui::TextStyle::Body);
        let (rect, _) = ui.allocate_exact_size(vec2(h, h), Sense::hover());
        let point = if s.sort.desc { Point::Down } else { Point::Up };
        triangle(ui, colors.fg(Token::Header), rect, point);
    }
    if r.clicked() {
        out.push(Intent::Act(match key {
            SortKey::Size => Action::SortSize,
            SortKey::Name => Action::SortName,
            SortKey::Items => Action::SortItems,
            SortKey::Mtime => Action::SortMtime,
        }));
    }
}

/// The cells of entry `e` on row `i`.
fn entry(
    row: &mut TableRow<'_, '_>,
    s: &SessionState,
    colors: &Colors,
    e: &EntryData,
    i: usize,
    out: &mut Vec<Intent>,
) {
    let dim = |text: String| colors.text(text, Token::FgDim);
    let size_token = match e.mark {
        Mark::Scanning | Mark::Rescanning | Mark::Stalled | Mark::Incomplete | Mark::Error => {
            Token::FgDim
        }
        _ => Token::Fg,
    };
    let marked = marks::is_marked(s, e);
    row.col(|ui| mark(ui, s, colors, e, marked, i, out));
    row.col(|ui| {
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            let token = if marked { Token::Mark } else { size_token };
            ui.label(colors.text(human(e.size), token));
        });
    });
    row.col(|ui| bar(ui, colors, ratio(e.size, s.list.rows_largest.max(1))));
    row.col(|ui| {
        // The data shared between entries has no items of its own.
        if !(e.pseudo && e.fold == 0) {
            ui.label(dim(count(e.items)));
        }
    });
    row.col(|ui| {
        if e.mtime > 0 && !e.pseudo {
            ui.label(dim(dfdu_tree::day(e.mtime)));
        }
    });
    row.col(|ui| name(ui, s, colors, e, i, out));
}

/// The mark of entry `e` on row `i`: a dot when it is marked, a ring under the pointer; a click
/// marks or unmarks it. A read-only browser marks nothing and draws none; nor do rows that are
/// not entries (the data shared between entries, fold rows).
fn mark(
    ui: &mut Ui,
    s: &SessionState,
    colors: &Colors,
    e: &EntryData,
    marked: bool,
    i: usize,
    out: &mut Vec<Intent>,
) {
    if s.read_only || e.pseudo || e.fold > 0 {
        return;
    }
    let h = ui.text_style_height(&egui::TextStyle::Body);
    let (rect, r) = ui.allocate_exact_size(vec2(h, h), Sense::click());
    let (c, radius) = (rect.center(), h * 0.28);
    if marked {
        ui.painter()
            .circle_filled(c, radius, colors.fg(Token::Mark));
    } else if r.hovered() {
        let stroke = Stroke::new(1.0, colors.fg(Token::FgDim));
        ui.painter().circle_stroke(c, radius, stroke);
    }
    let what = if marked { "Unmark" } else { "Mark" };
    r.widget_info(|| egui::WidgetInfo::selected(egui::Role::CheckBox, true, marked, what));
    if r.clicked() {
        out.push(Intent::Mark(i));
    }
}

/// The size bar: the part of the largest row's size `frac` is.
fn bar(ui: &mut Ui, colors: &Colors, frac: f64) {
    let h = ui.text_style_height(&egui::TextStyle::Body) * 0.6;
    let (rect, _) = ui.allocate_exact_size(vec2(BAR_WIDTH - 8.0, h), Sense::hover());
    let p = ui.painter();
    p.rect_filled(rect, 2.0, colors.fg(Token::BarEmpty));
    #[expect(clippy::cast_possible_truncation, reason = "a width on screen")]
    let w = rect.width() * frac.clamp(0.0, 1.0) as f32;
    let filled = Rect::from_min_size(rect.min, vec2(w, rect.height()));
    p.rect_filled(filled, 2.0, colors.fg(Token::BarFill));
}

/// The name cell: in tree mode indented by depth, with a directory's triangle; a note after the
/// name for a mark.
fn name(
    ui: &mut Ui,
    s: &SessionState,
    colors: &Colors,
    e: &EntryData,
    i: usize,
    out: &mut Vec<Intent>,
) {
    let h = ui.text_style_height(&egui::TextStyle::Body);
    if let Some(tree) = &s.tree {
        ui.add_space(f32::from(e.depth) * h * INDENT);
        let (rect, r) = ui.allocate_exact_size(vec2(h, h), Sense::click());
        if e.is_dir && !e.pseudo {
            let open = tree.is_expanded(e.node);
            let token = if r.hovered() { Token::Fg } else { Token::FgDim };
            let point = if open { Point::Down } else { Point::Right };
            triangle(ui, colors.fg(token), rect, point);
            let what = if open { "Collapse" } else { "Expand" };
            r.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, true, what));
            if r.clicked() {
                out.push(Intent::ToggleRow(i));
            }
        }
    }
    let text = String::from_utf8_lossy(&e.name);
    let label = if e.fold > 0 {
        colors.text(text, Token::FgDim).italics()
    } else if e.pseudo {
        colors.text(text, Token::FgDim)
    } else if e.is_dir {
        colors.text(format!("{text}/"), Token::Dir)
    } else {
        colors.text(text, Token::Fg)
    };
    let label = if e.ghost {
        label.strikethrough()
    } else {
        label
    };
    // Not selectable, so a click on the name reaches the row.
    ui.add(egui::Label::new(label).selectable(false).truncate());
    if e.fold == 0
        && let Some((note, token)) = mark_note(e)
    {
        ui.label(colors.text(note, token));
    }
}

/// Where a triangle points.
#[derive(Copy, Clone)]
enum Point {
    Up,
    Down,
    Right,
}

/// A triangle in `rect` pointing to `point`: a directory's in tree mode (down when it is
/// expanded), or the sorted column's (down for the largest, latest or last first).
fn triangle(ui: &Ui, color: egui::Color32, rect: Rect, point: Point) {
    let c = rect.center();
    let (r, n) = (rect.height() * 0.25, rect.height() * 0.15);
    let points = match point {
        Point::Up => vec![
            pos2(c.x - r, c.y + n),
            pos2(c.x + r, c.y + n),
            pos2(c.x, c.y - r),
        ],
        Point::Down => vec![
            pos2(c.x - r, c.y - n),
            pos2(c.x + r, c.y - n),
            pos2(c.x, c.y + r),
        ],
        Point::Right => vec![
            pos2(c.x - n, c.y - r),
            pos2(c.x + r, c.y),
            pos2(c.x - n, c.y + r),
        ],
    };
    ui.painter()
        .add(Shape::convex_polygon(points, color, Stroke::NONE));
}

/// What the name cell says after the name of an entry with a mark.
fn mark_note(e: &EntryData) -> Option<(String, Token)> {
    let dim = |t: &str| Some((t.to_owned(), Token::FgDim));
    match e.mark {
        Mark::None => None,
        Mark::Scanning | Mark::Rescanning => dim("scanning"),
        Mark::Stalled => Some(("not responding".into(), Token::StatusWarn)),
        Mark::Error => Some((
            e.problem
                .clone()
                .unwrap_or_else(|| "could not be read".into()),
            Token::StatusError,
        )),
        Mark::OtherFs => dim("another filesystem, not scanned"),
        Mark::Network => dim("network filesystem, not scanned"),
        Mark::Kernel => dim("kernel filesystem"),
        Mark::DupMount => dim("mounted again, counted elsewhere"),
        Mark::ErrorsInside => Some((
            format!(
                "{} error{} inside",
                count(e.errors_inside),
                plural(e.errors_inside)
            ),
            Token::StatusError,
        )),
        Mark::Deleting => Some(("deleting".into(), Token::StatusWarn)),
        Mark::Incomplete => Some((
            format!("incomplete: rescan with {}", first_key(Action::Rescan)),
            Token::StatusWarn,
        )),
    }
}

/// The summary row of the entries new since the last sort; a click sorts them in.
fn new_entries(
    row: &mut TableRow<'_, '_>,
    s: &SessionState,
    colors: &Colors,
    out: &mut Vec<Intent>,
) {
    let n = s.list.new_count as u64;
    row.col(|_| {});
    row.col(|ui| {
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            ui.label(colors.text(human(s.list.new_largest), Token::FgDim));
        });
    });
    row.col(|_| {});
    row.col(|_| {});
    row.col(|_| {});
    row.col(|ui| {
        let text = format!(
            "{} new entr{} since the last sort ({} sorts them in)",
            count(n),
            if n == 1 { "y" } else { "ies" },
            first_key(Action::Resort)
        );
        ui.label(colors.text(text, Token::FgDim).italics());
    });
    if row.response().clicked() {
        out.push(Intent::Act(Action::Resort));
    }
}
