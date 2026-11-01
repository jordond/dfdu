//! The Cleanup view (`3`): every match of the rules in the whole scan, as the terminal's view
//! lists them. A summary bar says what is suggested and ticked, with the levels shown and the
//! button that ticks the suggestion. Sections by safety level, each saying what the level
//! means, hold the categories found at that level, which open to their items: the tick, the
//! path, why it is suggested or not, the category, the size and the day modified. The first
//! opening ticks the suggested items (not in a read-only browser); `f` (or the level picked in
//! the summary bar) shows only the safer levels, `A` ticks the suggested items again and `N`
//! opens the selected item's native command (`pane`). A right click opens a row's menu
//! (`menu::review`). Below the table, the selected row's rule (`detail`). What is listed,
//! ticked and run is the session's (`dfdu_session::cleanup`): a click or a key here is one of
//! its intents.

use dfdu_engine::{Verdict, tilde};
use dfdu_rules::CategoryIx;
use dfdu_session::SessionState;
use dfdu_session::actions::{Action, first_key};
use dfdu_session::cleanup::{self, Intent as Ask, Item, Row};
use dfdu_session::format::{count, human};
use dfdu_session::palette::Token;
use dfdu_tree::{Safety, now_secs};
use egui::{Align, Label, Layout, Sense, Ui, vec2};
use egui_extras::{Column, TableBuilder, TableRow};

use crate::browser::{Intent, ViewState};
use crate::kit::{self, Kind};
use crate::list::{self, Ink, Point};
use crate::menu;
use crate::review::{self, ROW, Tick, right};
use crate::theme::{Colors, style};

mod detail;
pub mod pane;

/// The levels the filter shows, in the order `f` steps through them, with their names.
const LEVELS: [(Option<Safety>, &str); 4] = [
    (None, "Everything"),
    (Some(Safety::Safe), "Safe only"),
    (Some(Safety::Regenerable), "Safe and regenerable"),
    (Some(Safety::Review), "Up to review"),
];

/// A safety level as a section: its name and what it means for the user.
pub fn section_name(s: Safety) -> (&'static str, &'static str) {
    match s {
        Safety::Safe => ("Safe to delete", "caches the owning program recreates"),
        Safety::Regenerable => ("Regenerable", "rebuilt or reinstalled when needed"),
        Safety::Review => ("Review first", "may hold work"),
        Safety::Manual => ("Manual", "remove with the owner's own tool"),
        Safety::Protected => ("Protected", "never deleted"),
    }
}

/// Why an item is or is not suggested, in a word; the detail below the table has the sentence.
fn chip(v: Verdict) -> &'static str {
    match v {
        Verdict::Safe | Verdict::Stale(_) => "suggested",
        Verdict::Newer(_) => "recent",
        Verdict::Below(_) => "small",
        Verdict::InUse => "in use",
        Verdict::Scanning => "scanning",
        Verdict::Level(_) | Verdict::Tracked | Verdict::VcsUnknown => "review",
    }
}

/// The names of the table's columns, as the accessibility tree has them.
const COLUMNS: [&str; 6] = ["Ticked", "Path", "Why", "Category", "Size", "Modified"];

/// The height of a section's row: a header, with room above it.
const SECTION: f32 = 34.0;

/// Draws the view and returns how many rows it had room for.
pub fn show(
    ui: &mut Ui,
    s: &SessionState,
    colors: &Colors,
    view: &mut ViewState,
    out: &mut Vec<Intent>,
) -> usize {
    let rows = cleanup::rows(s);
    if rows.is_empty() && s.cleanup.filter.is_none() {
        let visible = list::visible_rows(ui);
        let lines = [
            "Lists what the rules recognise in the whole scan (caches, build output, \
             dependencies), by how safe it is to delete.",
            if s.snap.busy {
                "Nothing matched yet; the scan is still running."
            } else {
                "Nothing the rules know about was found."
            },
        ];
        review::empty(ui, colors, &lines, |_| {});
        return visible;
    }
    summary(ui, s, colors, out);
    notes(ui, s, colors);
    let cursor = s.cleanup.cursor.min(rows.len().saturating_sub(1));
    egui::Panel::bottom("dfdu-cleanup-detail")
        .resizable(false)
        .frame(egui::Frame::new().inner_margin(egui::Margin::symmetric(16, 10)))
        .show(ui, |ui| {
            detail::show(ui, s, colors, rows.get(cursor).copied(), out);
        });
    let visible = list::visible_rows(ui);
    if rows.is_empty() {
        review::empty(ui, colors, &["Nothing at these levels."], |_| {});
        return visible;
    }
    table(ui, s, colors, view, &rows, cursor, out);
    visible
}

/// The summary bar: what is suggested, what is ticked of what was found, the levels shown and
/// the button that ticks the suggestion.
fn summary(ui: &mut Ui, s: &SessionState, colors: &Colors, out: &mut Vec<Intent>) {
    let items = &s.extras.cleanup.items;
    let now = now_secs();
    let found: u64 = items.iter().map(|it| it.size).sum();
    let suggested: u64 = (items.iter())
        .filter(|it| cleanup::markable(it))
        .filter(|it| cleanup::recommended(s, it, now).is_some_and(Verdict::recommended))
        .map(|it| it.size)
        .sum();
    let ticked: Vec<&Item> = (items.iter())
        .filter(|it| cleanup::markable(it) && cleanup::is_marked(s, it))
        .collect();
    let ticked_size: u64 = ticked.iter().map(|it| it.size).sum();
    let line = format!(
        "{} ticked ({}) of {} items · {} found",
        count(ticked.len() as u64),
        human(ticked_size),
        count(items.len() as u64),
        human(found)
    );
    review::summary(ui, colors, (&human(suggested), "suggested"), &line, |ui| {
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
        kit::key_cap(ui, colors, first_key(Action::CleanupFilter));
        let at = LEVELS
            .iter()
            .position(|(f, _)| *f == s.cleanup.filter)
            .unwrap_or(0);
        let names = LEVELS.map(|(_, name)| name);
        if let Some(i) = kit::segmented(ui, colors, "Levels shown", &names, at) {
            // The session steps through the levels in this order, round the end.
            for _ in 0..(i + LEVELS.len() - at) % LEVELS.len() {
                out.push(Intent::Cleanup(Ask::CycleFilter));
            }
        }
    });
}

/// What native commands count themselves, under the summary.
fn notes(ui: &mut Ui, s: &SessionState, colors: &Colors) {
    let native = &s.extras.cleanup.native_sizes;
    if native.is_empty() {
        return;
    }
    let parts: Vec<String> = (native.iter())
        .map(|(name, r)| match r {
            Ok(b) => format!("{name}: {} by its own count", human(*b)),
            Err(e) => format!("{name}: {e}"),
        })
        .collect();
    review::note(ui, colors, &parts.join(" · "));
}

/// The rows, in a table that lays out only those on screen.
fn table(
    ui: &mut Ui,
    s: &SessionState,
    colors: &Colors,
    view: &mut ViewState,
    rows: &[Row],
    cursor: usize,
    out: &mut Vec<Intent>,
) {
    list::whole_rows(ui);
    let category = ui.available_width() > 720.0;
    let mut table = TableBuilder::new(ui)
        .id_salt("dfdu-cleanup")
        .striped(false)
        .sense(Sense::click())
        .cell_layout(Layout::left_to_right(Align::Center))
        .column(Column::exact(64.0))
        .column(Column::remainder().at_least(160.0).clip(true))
        .column(Column::exact(80.0).clip(true))
        .column(Column::exact(if category { 170.0 } else { 0.0 }).clip(true))
        .column(Column::exact(96.0))
        .column(Column::exact(96.0))
        .min_scrolled_height(0.0)
        .auto_shrink(false);
    if std::mem::take(&mut view.follow) {
        table = table.scroll_to_row(cursor, None);
    }
    let now = now_secs();
    let heights = rows.iter().map(|r| match r {
        Row::Section(_) => SECTION,
        Row::Group { .. } | Row::Item(_) => ROW,
    });
    table.body(|body| {
        body.heterogeneous_rows(heights, |mut row| {
            let i = row.index();
            row.set_selected(i == cursor);
            let ink = Ink::new(colors, i == cursor);
            let tick = match rows[i] {
                Row::Section(safety) => section(&mut row, s, colors, &ink, safety, i, out),
                Row::Group { cat, safety } => {
                    group(&mut row, s, colors, &ink, (cat, safety), i, out)
                }
                Row::Item(ix) => item(&mut row, s, colors, &ink, (ix, category), i, now, out),
            };
            let r = row.response();
            list::name_row(&r, i == cursor, row_name(s, rows[i]));
            if r.double_clicked() {
                out.push(Intent::Cleanup(Ask::Select(i)));
                out.push(Intent::Cleanup(Ask::Activate));
            } else if r.clicked() || r.secondary_clicked() {
                out.push(Intent::Cleanup(Ask::Select(i)));
            }
            let path = matches!(rows[i], Row::Item(_));
            r.context_menu(|ui| {
                menu::review(ui, s, path, (tick, &toggle(i)), out);
            });
        });
    });
}

/// What row `row` is called in the accessibility tree: its level, category or path, and its
/// size.
fn row_name(s: &SessionState, row: Row) -> String {
    let items = &s.extras.cleanup.items;
    let (name, ix) = match row {
        Row::Section(safety) => (
            section_name(safety).0.to_owned(),
            cleanup::section_items(s, safety),
        ),
        Row::Group { cat, safety } => (
            cat.get().name.to_owned(),
            cleanup::group_items(s, cat, safety),
        ),
        Row::Item(i) => (tilde(&items[i].path, s.home.as_deref()), vec![i]),
    };
    let size: u64 = ix.iter().map(|&i| items[i].size).sum();
    format!("{name}, {}", human(size))
}

/// What ticking row `i` asks, as its tick does: the cursor stays on the row.
fn toggle(i: usize) -> [Intent; 3] {
    [
        Intent::Cleanup(Ask::Select(i)),
        Intent::Cleanup(Ask::Toggle),
        Intent::Cleanup(Ask::Select(i)),
    ]
}

/// The tick of items `ix`: on when every one that can be ticked is.
fn tick_of_items(s: &SessionState, ix: &[usize]) -> Tick {
    let items = &s.extras.cleanup.items;
    let can: Vec<&Item> = (ix.iter())
        .map(|&i| &items[i])
        .filter(|it| cleanup::markable(it))
        .collect();
    if can.is_empty() {
        return Tick::Locked;
    }
    let ticked = can.iter().filter(|it| cleanup::is_marked(s, it)).count();
    review::tick_of(ticked, can.len())
}

/// The first cell of a section or category on row `i`, ticked as `tick` says: its triangle,
/// which opens or closes it, and its tick, which ticks every item in it; a category's indented.
#[expect(clippy::too_many_arguments, reason = "a row's state")]
fn opener(
    ui: &mut Ui,
    s: &SessionState,
    colors: &Colors,
    ink: &Ink,
    row: Row,
    tick: Tick,
    i: usize,
    out: &mut Vec<Intent>,
) {
    if matches!(row, Row::Group { .. }) {
        ui.add_space(style::SPACE_3);
    }
    let (rect, r) = ui.allocate_exact_size(vec2(16.0, 16.0), Sense::click());
    let open = cleanup::is_open(s, row);
    let token = if r.hovered() { Token::Fg } else { Token::FgDim };
    let point = if open { Point::Down } else { Point::Right };
    list::triangle(ui, ink.of(colors.fg(token)), rect, point);
    let what = if open { "Collapse" } else { "Expand" };
    r.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, true, what));
    if r.clicked() {
        out.push(Intent::Cleanup(Ask::Select(i)));
        out.push(Intent::Cleanup(Ask::Activate));
    }
    if !s.read_only && review::tick(ui, colors, tick).clicked() {
        out.push(Intent::Cleanup(Ask::Select(i)));
        out.push(Intent::Cleanup(Ask::Toggle));
    }
}

/// The size of items `ix`.
fn total(s: &SessionState, ix: &[usize]) -> u64 {
    ix.iter().map(|&i| s.extras.cleanup.items[i].size).sum()
}

/// Adds `n` empty cells to `row`.
fn skip(row: &mut TableRow<'_, '_>, n: usize) {
    for _ in 0..n {
        list::cell(row, &COLUMNS, |_| {});
    }
}

/// A safety level's row, a header in its colour with its glyph: `✓ Safe to delete`, what the
/// level means, and its total; a rule in `border` under it. Returns its tick.
fn section(
    row: &mut TableRow<'_, '_>,
    s: &SessionState,
    colors: &Colors,
    ink: &Ink,
    safety: Safety,
    i: usize,
    out: &mut Vec<Intent>,
) -> Tick {
    let ix = cleanup::section_items(s, safety);
    let tick = tick_of_items(s, &ix);
    let (glyph, token) = crate::details::safety(safety);
    let color = ink.of(colors.fg(token));
    let dim = ink.of(colors.fg(Token::FgDim));
    list::cell(row, &COLUMNS, |ui| {
        review::rule_under(ui, colors.fg(Token::Border));
        opener(ui, s, colors, ink, Row::Section(safety), tick, i, out);
    });
    list::cell(row, &COLUMNS, |ui| {
        let (name, meaning) = section_name(safety);
        ui.spacing_mut().item_spacing.x = 6.0;
        review::glyph(ui, glyph, 14.0, color);
        ui.label(kit::title(name, 14.0).color(color));
        ui.add_space(style::SPACE_1);
        ui.add(
            Label::new(kit::sans(meaning, 12.0).color(dim))
                .selectable(false)
                .truncate(),
        );
    });
    skip(row, 2);
    list::cell(row, &COLUMNS, |ui| {
        right(ui, kit::mono_bold(human(total(s, &ix)), 12.0).color(dim));
    });
    list::cell(row, &COLUMNS, |ui| {
        let n = ix.len() as u64;
        let what = if n == 1 { "item" } else { "items" };
        right(
            ui,
            kit::sans(format!("{} {what}", count(n)), 12.0).color(dim),
        );
    });
    tick
}

/// A category's row in its section: its chip, how many items, and their total. Returns its
/// tick.
fn group(
    row: &mut TableRow<'_, '_>,
    s: &SessionState,
    colors: &Colors,
    ink: &Ink,
    (cat, safety): (CategoryIx, Safety),
    i: usize,
    out: &mut Vec<Intent>,
) -> Tick {
    let ix = cleanup::group_items(s, cat, safety);
    let r = Row::Group { cat, safety };
    let tick = tick_of_items(s, &ix);
    let dim = ink.of(colors.fg(Token::FgDim));
    list::cell(row, &COLUMNS, |ui| {
        opener(ui, s, colors, ink, r, tick, i, out)
    });
    list::cell(row, &COLUMNS, |ui| {
        let category = cat.get();
        let color = ink.of(colors.fg(list::category(category.color as usize)));
        kit::chip(ui, category.name, color);
    });
    skip(row, 2);
    list::cell(row, &COLUMNS, |ui| {
        right(ui, kit::mono(human(total(s, &ix)), 13.0).color(dim));
    });
    list::cell(row, &COLUMNS, |ui| {
        let n = ix.len() as u64;
        let what = if n == 1 { "item" } else { "items" };
        right(
            ui,
            kit::sans(format!("{} {what}", count(n)), 12.0).color(dim),
        );
    });
    tick
}

/// An item's row: its tick (disabled when it cannot be ticked), path, why it is suggested or
/// not, category, size and day modified. Returns its tick.
#[expect(clippy::too_many_arguments, reason = "a row's state")]
fn item(
    row: &mut TableRow<'_, '_>,
    s: &SessionState,
    colors: &Colors,
    ink: &Ink,
    (ix, category): (usize, bool),
    i: usize,
    now: i64,
    out: &mut Vec<Intent>,
) -> Tick {
    let it = &s.extras.cleanup.items[ix];
    let marked = cleanup::is_marked(s, it);
    let tick = if !cleanup::markable(it) {
        Tick::Locked
    } else if marked {
        Tick::On
    } else {
        Tick::Off
    };
    let dim = ink.of(colors.fg(Token::FgDim));
    list::cell(row, &COLUMNS, |ui| {
        ui.add_space(44.0);
        if s.read_only {
            return;
        }
        if tick == Tick::Locked {
            review::tick(ui, colors, tick);
        } else if review::tick(ui, colors, tick).clicked() {
            out.extend(toggle(i));
        }
    });
    list::cell(row, &COLUMNS, |ui| {
        let path = tilde(&it.path, s.home.as_deref());
        let token = if marked { Token::Mark } else { Token::Fg };
        // Not selectable, so a click on the path reaches the row.
        ui.add(
            Label::new(kit::mono(path, 13.0).color(ink.of(colors.fg(token))))
                .selectable(false)
                .truncate(),
        );
    });
    list::cell(row, &COLUMNS, |ui| {
        let verdict = cleanup::recommended(s, it, now);
        ui.label(kit::sans(verdict.map_or("", chip), 12.0).color(dim));
    });
    list::cell(row, &COLUMNS, |ui| {
        if let (true, Some(rules)) = (category, &s.snap.rules) {
            let c = rules.rule(it.class.rule).category();
            let color = ink.of(colors.fg(list::category(c.color as usize)));
            kit::chip(ui, c.name, color);
        }
    });
    list::cell(row, &COLUMNS, |ui| {
        let token = if marked { Token::Mark } else { Token::SizeBig };
        right(
            ui,
            kit::mono_bold(human(it.size), 13.0).color(ink.of(colors.fg(token))),
        );
    });
    list::cell(row, &COLUMNS, |ui| {
        if it.mtime > 0 {
            right(ui, kit::sans(dfdu_tree::day(it.mtime), 12.0).color(dim));
        }
    });
    tick
}
