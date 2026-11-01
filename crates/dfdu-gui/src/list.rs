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
use crate::theme::{Colors, style};
use crate::{kit, menu};

/// Which of the columns that drop at narrow widths are shown, and the share column's width.
#[derive(Clone, Copy)]
struct Shown {
    items: bool,
    modified: bool,
    category: bool,
    share: f32,
}

impl Shown {
    /// The columns `width` has room for beside a name of at least 160: the category first
    /// (only when a row has one), then the date, then the items go; the bar shrinks before.
    fn for_width(width: f32, any_category: bool) -> Shown {
        let fixed = 22.0 + 160.0 + 96.0 + 8.0 * 7.0;
        let mut room = width - fixed;
        let mut take = |w: f32| {
            let fits = room >= w;
            if fits {
                room -= w;
            }
            fits
        };
        let share_min = take(120.0);
        let items = take(64.0);
        let modified = take(104.0);
        let category = any_category && take(150.0);
        let share = if share_min {
            (120.0 + room).min(220.0)
        } else {
            0.0
        };
        Shown {
            items,
            modified,
            category,
            share,
        }
    }

    /// The names of the columns shown, as the accessibility tree has them.
    fn names(self) -> Vec<&'static str> {
        let mut n = vec!["Marked", "Name", "Size"];
        if self.share > 0.0 {
            n.push("Share");
        }
        if self.items {
            n.push("Items");
        }
        if self.modified {
            n.push("Modified");
        }
        if self.category {
            n.push("Category");
        }
        n
    }
}
/// Width of the share column: the bar and the percentage.
pub const BAR_WIDTH: f32 = 200.0;
/// Indent of each tree level.
const INDENT: f32 = 16.0;
/// The largest sizes of a listing stand out: the first this many rows sorted by size.
const BIG: usize = 3;

/// Draws the list and returns how many rows it had room for.
pub fn show(
    ui: &mut Ui,
    s: &SessionState,
    colors: &Colors,
    view: &mut ViewState,
    out: &mut Vec<Intent>,
) -> usize {
    let row_h = row_height(ui);
    let visible = visible_rows(ui);
    if s.list.rows.is_empty() && s.list.new_count == 0 {
        empty(ui, colors, &empty_note(s));
        return visible;
    }
    let selected = s.selected_index();
    let any_category =
        s.snap.rules.is_some() && s.list.rows.iter().any(|&r| s.snap.entries[r].tag.is_some());
    let shown = Shown::for_width(ui.available_width(), any_category);
    let names = shown.names();
    whole_rows(ui);
    header_rule(ui, colors, row_h);
    let mut table = TableBuilder::new(ui)
        .id_salt(("list", &s.snap.dir_path))
        .striped(false)
        .sense(Sense::click())
        .cell_layout(Layout::left_to_right(Align::Center))
        .column(Column::exact(22.0))
        .column(Column::remainder().at_least(160.0).clip(true))
        .column(Column::exact(96.0));
    if shown.share > 0.0 {
        table = table.column(Column::exact(shown.share));
    }
    if shown.items {
        table = table.column(Column::exact(64.0));
    }
    if shown.modified {
        table = table.column(Column::exact(104.0));
    }
    if shown.category {
        table = table.column(Column::exact(150.0).clip(true));
    }
    let mut table = table.min_scrolled_height(0.0).auto_shrink(false);
    if std::mem::take(&mut view.follow)
        && let Some(i) = selected
    {
        table = table.scroll_to_row(i, None);
    }
    let new_row = usize::from(s.list.new_count > 0);
    table
        .header(row_h, |mut h| {
            for (key, label, name, right) in [
                (None, "", "Marked", false),
                (Some(SortKey::Name), "Name", "Name", false),
                (Some(SortKey::Size), "Size", "Size", true),
                (None, "", "Share", false),
                (Some(SortKey::Items), "Items", "Items", true),
                (Some(SortKey::Mtime), "Modified", "Modified", false),
                (None, "Category", "Category", false),
            ] {
                if !names.contains(&name) {
                    continue;
                }
                h.col(|ui| {
                    name_column(ui, name);
                    if name == "Modified" {
                        ui.add_space(12.0);
                    }
                    header(ui, s, colors, key, label, right, out);
                });
            }
        })
        .body(|body| {
            body.rows(row_h, s.list.rows.len() + new_row, |mut row| {
                let i = row.index();
                if i == s.list.rows.len() {
                    new_entries(&mut row, names.len(), s, colors, out);
                    return;
                }
                row.set_selected(selected == Some(i));
                let e = &s.snap.entries[s.list.rows[i]];
                entry(
                    &mut row,
                    s,
                    colors,
                    (shown, &names),
                    e,
                    i,
                    selected == Some(i),
                    out,
                );
                let r = row.response();
                let slash = if e.is_dir && !e.pseudo { "/" } else { "" };
                let name = String::from_utf8_lossy(&e.name);
                name_row(
                    &r,
                    selected == Some(i),
                    format!("{name}{slash}, {}", human(e.size)),
                );
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

/// The rule in `border` under a table's header row of height `row_h`, across the sheet.
pub fn header_rule(ui: &Ui, colors: &Colors, row_h: f32) {
    let top = ui.cursor().top();
    let x = ui.max_rect().x_range();
    ui.painter().hline(
        x,
        top + row_h + ui.spacing().item_spacing.y / 2.0,
        Stroke::new(1.0, colors.fg(Token::Border)),
    );
}

/// What an empty view says, centred in it.
pub fn empty(ui: &mut Ui, colors: &Colors, note: &str) {
    ui.centered_and_justified(|ui| {
        ui.label(kit::sans(note, 13.0).color(colors.fg(Token::FgDim)));
    });
}

/// Makes the labels added to `ui` from now on take no clicks, so that a click or a right click
/// anywhere on a table's row reaches the row.
pub fn whole_rows(ui: &mut Ui) {
    ui.style_mut().interaction.selectable_labels = false;
}

/// Adds the next cell of `row` with `add`, named for its column in `columns` in the
/// accessibility tree: the table's cells take clicks, so each must have a name.
pub fn cell(row: &mut TableRow<'_, '_>, columns: &[&str], add: impl FnOnce(&mut Ui)) {
    let name = columns.get(row.col_index()).copied().unwrap_or_default();
    row.col(|ui| {
        let r = ui.response();
        r.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Cell, true, name));
        add(ui);
    });
}

/// Names the header cell `ui` of a column `name` in the accessibility tree, a column without a
/// title too: the table's cells take clicks, so each must have a name.
pub fn name_column(ui: &Ui, name: &str) {
    let r = ui.response();
    r.widget_info(|| egui::WidgetInfo::labeled(egui::Role::ColumnHeader, true, name));
}

/// Names the row whose response is `r` in the accessibility tree: what it shows, in `label`,
/// and whether it is `selected`.
pub fn name_row(r: &egui::Response, selected: bool, label: String) {
    r.widget_info(|| egui::WidgetInfo::selected(egui::Role::Row, true, selected, &label));
}

/// The height of a row of the list and of the other views' tables.
pub fn row_height(_ui: &Ui) -> f32 {
    style::ROW
}

/// The rows a table under a header row has room for in `ui`: what a page moves by.
pub fn visible_rows(ui: &Ui) -> usize {
    let row_h = row_height(ui);
    let room = ui.available_height() - row_h - ui.spacing().item_spacing.y;
    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "a count of rows on screen"
    )]
    let visible = (room / (row_h + ui.spacing().item_spacing.y))
        .floor()
        .max(0.0) as usize;
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

/// A column's header, in the label style: a click sorts by it, again reverses; the sorted
/// one in `header` with its arrow.
fn header(
    ui: &mut Ui,
    s: &SessionState,
    colors: &Colors,
    key: Option<SortKey>,
    label: &str,
    right: bool,
    out: &mut Vec<Intent>,
) {
    if right {
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            header_text(ui, s, colors, key, label, out);
        });
    } else {
        header_text(ui, s, colors, key, label, out);
    }
}

fn header_text(
    ui: &mut Ui,
    s: &SessionState,
    colors: &Colors,
    key: Option<SortKey>,
    label: &str,
    out: &mut Vec<Intent>,
) {
    let dim = colors.fg(Token::FgDim);
    let Some(key) = key else {
        if !label.is_empty() {
            ui.label(kit::label(label, 12.0).color(dim));
        }
        return;
    };
    let sorted = s.sort.key == key;
    let text = if sorted {
        let arrow = if s.sort.desc { "↓" } else { "↑" };
        kit::title(format!("{label} {arrow}"), 12.0).color(colors.fg(Token::Header))
    } else {
        kit::label(label, 12.0).color(dim)
    };
    let r = ui.add(
        egui::Button::new(text)
            .frame(false)
            .min_size(vec2(0.0, 0.0)),
    );
    if r.clicked() {
        out.push(Intent::Act(match key {
            SortKey::Size => Action::SortSize,
            SortKey::Name => Action::SortName,
            SortKey::Items => Action::SortItems,
            SortKey::Mtime => Action::SortMtime,
        }));
    }
}

/// The colours of a row's text: every text in the selected row is `cursor`.
pub struct Ink {
    pub on: bool,
    cursor: egui::Color32,
}

impl Ink {
    pub fn new(colors: &Colors, on: bool) -> Self {
        Ink {
            on,
            cursor: colors.fg(Token::Cursor),
        }
    }

    /// `color`, or `cursor` on the selected row.
    pub fn of(&self, color: egui::Color32) -> egui::Color32 {
        if self.on { self.cursor } else { color }
    }
}

/// The cells of entry `e` on row `i`.
#[expect(clippy::too_many_arguments, reason = "a row's state")]
fn entry(
    row: &mut TableRow<'_, '_>,
    s: &SessionState,
    colors: &Colors,
    (shown, names): (Shown, &[&str]),
    e: &EntryData,
    i: usize,
    on: bool,
    out: &mut Vec<Intent>,
) {
    let ink = Ink::new(colors, on);
    let dim = ink.of(colors.fg(Token::FgDim));
    let pending = matches!(
        e.mark,
        Mark::Scanning | Mark::Rescanning | Mark::Stalled | Mark::Incomplete | Mark::Error
    );
    let marked = marks::is_marked(s, e);
    cell(row, names, |ui| mark(ui, s, colors, e, marked, i, out));
    cell(row, names, |ui| {
        name(ui, s, colors, &ink, e, marked, i, out)
    });
    cell(row, names, |ui| {
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            let big = s.sort.key == SortKey::Size && s.sort.desc && i < BIG && !e.pseudo;
            let text = human(e.size);
            let t = if marked {
                kit::mono_bold(text, 13.0).color(ink.of(colors.fg(Token::Mark)))
            } else if pending {
                kit::mono(text, 13.0).color(dim)
            } else if big || on {
                kit::mono_bold(text, 13.0).color(ink.of(colors.fg(Token::SizeBig)))
            } else {
                kit::mono(text, 13.0).color(colors.fg(Token::Fg))
            };
            ui.label(t);
        });
    });
    if shown.share > 0.0 {
        cell(row, names, |ui| {
            let total = s.size_mode.of(&s.snap.dir_total).max(1);
            share(
                ui,
                colors,
                &ink,
                ratio(e.size, s.list.rows_largest.max(1)),
                ratio(e.size, total),
            );
        });
    }
    if shown.items {
        cell(row, names, |ui| {
            // The data shared between entries has no items of its own.
            if !(e.pseudo && e.fold == 0) {
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    ui.label(kit::mono(count(e.items), 13.0).color(dim));
                });
            }
        });
    }
    if shown.modified {
        cell(row, names, |ui| {
            if e.mtime > 0 && !e.pseudo {
                ui.add_space(12.0);
                ui.label(kit::mono(dfdu_tree::day(e.mtime), 12.0).color(dim));
            }
        });
    }
    if !shown.category {
        return;
    }
    cell(row, names, |ui| {
        let rules = s.snap.rules.as_deref();
        if let Some((tag, rules)) = e.tag.zip(rules) {
            let (_, slot) = tag.category(rules);
            let name = rules.rule(tag.class.rule).category().name;
            // Inside a match, rows show its category dim: the match is the directory above.
            let color = if tag.inherited {
                dim
            } else {
                ink.of(colors.fg(category(slot)))
            };
            kit::chip(ui, name, color);
        }
    });
}

/// The colour of category slot `slot`.
pub fn category(slot: usize) -> Token {
    dfdu_session::palette::Category::ALL
        .get(slot)
        .map_or(Token::CatOther, |c| c.token())
}

/// The share cell: a bar of `frac` of the largest row, then the share of the directory.
fn share(ui: &mut Ui, colors: &Colors, ink: &Ink, frac: f64, of_dir: f64) {
    let pct_w = 48.0;
    let w = (ui.available_width() - pct_w - 8.0).max(8.0);
    let (rect, _) = ui.allocate_exact_size(vec2(w, style::ROW), Sense::hover());
    let bar = Rect::from_center_size(rect.center(), vec2(w, 8.0));
    kit::paint_bar(ui, colors, bar, frac);
    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
        let pct = format!("{:.1}%", of_dir * 100.0);
        ui.label(kit::mono(pct, 11.0).color(ink.of(colors.fg(Token::FgDim))));
    });
}

/// The mark of entry `e` on row `i`: a check box in `mark`; a click marks or unmarks it. A
/// read-only browser marks nothing and draws none; nor do rows that are not entries (the data
/// shared between entries, fold rows).
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
    if dot(ui, colors, marked).clicked() {
        out.push(Intent::Mark(i));
    }
}

/// A mark's check box: filled in `mark` with a tick when `marked`, outlined otherwise, named for
/// what a click does.
pub fn dot(ui: &mut Ui, colors: &Colors, marked: bool) -> egui::Response {
    let (rect, r) = ui.allocate_exact_size(vec2(16.0, 16.0), Sense::click());
    let b = Rect::from_center_size(rect.center(), vec2(14.0, 14.0));
    let radius = egui::CornerRadius::same(style::RADIUS_SM);
    let p = ui.painter();
    if marked {
        p.rect_filled(b, radius, colors.fg(Token::Mark));
        let c = b.center();
        let tick = vec![
            pos2(c.x - 3.5, c.y),
            pos2(c.x - 1.0, c.y + 2.6),
            pos2(c.x + 3.6, c.y - 2.8),
        ];
        p.add(Shape::line(tick, Stroke::new(1.8, colors.chrome.surface)));
    } else {
        let edge = if r.hovered() {
            colors.fg(Token::BorderFocus)
        } else {
            colors.fg(Token::Border)
        };
        p.rect_stroke(b, radius, Stroke::new(1.0, edge), egui::StrokeKind::Inside);
    }
    let what = if marked { "Unmark" } else { "Mark" };
    r.widget_info(|| egui::WidgetInfo::selected(egui::Role::CheckBox, true, marked, what));
    r
}

/// The size bar: the part of the largest row's size `frac` is.
pub fn bar(ui: &mut Ui, colors: &Colors, frac: f64) {
    kit::size_bar(ui, colors, 72.0, 8.0, frac);
}

/// The name cell in mono: in tree mode indented by depth, with a directory's triangle;
/// directories in `dir` with a slash, marked entries in `mark`; a note after the name for a
/// mark.
#[expect(clippy::too_many_arguments, reason = "a row's state")]
fn name(
    ui: &mut Ui,
    s: &SessionState,
    colors: &Colors,
    ink: &Ink,
    e: &EntryData,
    marked: bool,
    i: usize,
    out: &mut Vec<Intent>,
) {
    let h = 16.0;
    if let Some(tree) = &s.tree {
        ui.add_space(f32::from(e.depth) * INDENT);
        // Only a directory's triangle takes clicks; a file has room for one, and nothing in it.
        let opens = e.is_dir && !e.pseudo;
        let sense = if opens {
            Sense::click()
        } else {
            Sense::hover()
        };
        let (rect, r) = ui.allocate_exact_size(vec2(h, h), sense);
        if opens {
            let open = tree.is_expanded(e.node);
            let token = if r.hovered() { Token::Fg } else { Token::FgDim };
            let point = if open { Point::Down } else { Point::Right };
            triangle(ui, ink.of(colors.fg(token)), rect, point);
            let what = if open { "Collapse" } else { "Expand" };
            r.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, true, what));
            if r.clicked() {
                out.push(Intent::ToggleRow(i));
            }
        }
    }
    let text = String::from_utf8_lossy(&e.name);
    let dim = ink.of(colors.fg(Token::FgDim));
    let label = if e.fold > 0 {
        kit::mono(text, 13.0).color(dim)
    } else if e.pseudo {
        kit::mono(text, 13.0).color(dim).italics()
    } else if e.is_dir {
        let c = if marked { Token::Mark } else { Token::Dir };
        kit::mono_bold(format!("{text}/"), 13.0).color(ink.of(colors.fg(c)))
    } else {
        let c = if marked { Token::Mark } else { Token::Fg };
        kit::mono(text, 13.0).color(ink.of(colors.fg(c)))
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
        ui.label(kit::sans(note, 11.0).color(ink.of(colors.fg(token))));
    }
}

/// Where a triangle points.
#[derive(Copy, Clone)]
pub enum Point {
    Down,
    Right,
}

/// A triangle in `rect` pointing to `point`: a directory's in tree mode (down when it is
/// expanded) or a section's of a review view, or the sorted column's (down for the largest,
/// latest or last first).
pub fn triangle(ui: &Ui, color: egui::Color32, rect: Rect, point: Point) {
    let c = rect.center();
    let (r, n) = (rect.height() * 0.25, rect.height() * 0.15);
    let points = match point {
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
    columns: usize,
    s: &SessionState,
    colors: &Colors,
    out: &mut Vec<Intent>,
) {
    let n = s.list.new_count as u64;
    let dim = colors.fg(Token::FgDim);
    let names = ["Marked", "Name", "Size"];
    cell(row, &names, |_| {});
    cell(row, &names, |ui| {
        let text = format!(
            "{} new entr{} since the last sort ({} sorts them in)",
            count(n),
            if n == 1 { "y" } else { "ies" },
            first_key(Action::Resort)
        );
        ui.label(kit::sans(text, 12.0).color(dim).italics());
    });
    cell(row, &names, |ui| {
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            ui.label(kit::mono(human(s.list.new_largest), 13.0).color(dim));
        });
    });
    while row.col_index() < columns {
        row.col(|ui| {
            let r = ui.response();
            r.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Cell, true, "Empty"));
        });
    }
    let r = row.response();
    name_row(&r, false, format!("{} new since the last sort", count(n)));
    if r.clicked() {
        out.push(Intent::Act(Action::Resort));
    }
}
