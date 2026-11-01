//! The Cleanup view (`3`): every match of the rules in the whole scan, as the terminal's view
//! lists them. Sections by safety level, each saying what the level means, hold the categories
//! found at that level, which open to their items: the tick, size, entries, day modified, why
//! it is suggested or not, and the path. The first opening ticks the suggested items (not in a
//! read-only browser); `f` (or the level picked above the table) shows only the safer levels,
//! `A` ticks the suggested items again and `N` opens the selected item's native command
//! (`pane`). Below the table, the selected row's rule: what it is, what deleting it loses and
//! why it is suggested or not. What is listed, ticked and run is the session's
//! (`dfdu_session::cleanup`): a click or a key here is one of its intents.

use dfdu_engine::{Verdict, tilde};
use dfdu_rules::CategoryIx;
use dfdu_session::SessionState;
use dfdu_session::actions::{Action, first_key};
use dfdu_session::cleanup::{self, Intent as Ask, Item, Row};
use dfdu_session::format::{count, human};
use dfdu_session::palette::{Category, Token};
use dfdu_tree::{ClassFlags, Safety, now_secs};
use egui::{Align, Button, Label, Layout, RichText, Sense, Ui, vec2};
use egui_extras::{Column, TableBuilder, TableRow};

use crate::browser::{Intent, ViewState};
use crate::list::{self, Point};
use crate::review::{self, Tick};
use crate::theme::Colors;

pub mod pane;

/// The levels the filter shows, in the order `f` steps through them, with their names.
const LEVELS: [(Option<Safety>, &str); 4] = [
    (None, "Everything"),
    (Some(Safety::Safe), "Safe only"),
    (Some(Safety::Regenerable), "Safe and regenerable"),
    (Some(Safety::Review), "Up to review"),
];

/// A safety level as a section: its name and what it means for the user.
fn section_name(s: Safety) -> (&'static str, &'static str) {
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

/// The same as a sentence.
fn reason(v: Verdict) -> String {
    match v {
        Verdict::Safe => "Suggested: safe to delete.".into(),
        Verdict::Stale(d) => format!("Suggested: unchanged for over {d} days."),
        Verdict::Level(s) => format!("Not suggested: {} items never are.", s.as_str()),
        Verdict::InUse => "Not suggested: in use.".into(),
        Verdict::Tracked => "Not suggested: tracked by git.".into(),
        Verdict::VcsUnknown => "Not suggested: git could not tell if it is tracked.".into(),
        Verdict::Scanning => "Not suggested until its scan finishes.".into(),
        Verdict::Newer(d) => format!("Not suggested: changed in the last {d} days."),
        Verdict::Below(min) => format!("Not suggested: below the rule's {}.", human(min)),
    }
}

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
        let dim = |t: &str| colors.text(t, Token::FgDim);
        ui.label(dim(
            "Lists what the rules recognise in the whole scan (caches, build output, \
             dependencies), by how safe it is to delete.",
        ));
        ui.label(dim(if s.snap.busy {
            "Nothing matched yet; the scan is still running."
        } else {
            "Nothing the rules know about was found."
        }));
        return list::visible_rows(ui);
    }
    controls(ui, s, colors, out);
    notes(ui, s, colors);
    let cursor = s.cleanup.cursor.min(rows.len().saturating_sub(1));
    egui::Panel::bottom("dfdu-cleanup-detail")
        .resizable(false)
        .show(ui, |ui| detail(ui, s, colors, rows.get(cursor).copied()));
    let visible = list::visible_rows(ui);
    if rows.is_empty() {
        ui.label(colors.text("Nothing at these levels.", Token::FgDim));
        return visible;
    }
    table(ui, s, colors, view, &rows, cursor, out);
    visible
}

/// What the ticks add up to, the levels shown, and the buttons of the view.
fn controls(ui: &mut Ui, s: &SessionState, colors: &Colors, out: &mut Vec<Intent>) {
    let can: Vec<&Item> = (s.extras.cleanup.items.iter())
        .filter(|it| cleanup::markable(it))
        .collect();
    let all: u64 = can.iter().map(|it| it.size).sum();
    let ticked: Vec<&&Item> = can.iter().filter(|it| cleanup::is_marked(s, it)).collect();
    let ticked_size: u64 = ticked.iter().map(|it| it.size).sum();
    ui.horizontal(|ui| {
        ui.label(colors.text(
            format!("{} in {} items", human(all), count(can.len() as u64)),
            Token::Header,
        ));
        let token = if ticked.is_empty() {
            Token::FgDim
        } else {
            Token::Mark
        };
        ui.label(colors.text(
            format!(
                "{} ticked ({})",
                count(ticked.len() as u64),
                human(ticked_size)
            ),
            token,
        ));
        ui.separator();
        let at = LEVELS
            .iter()
            .position(|(f, _)| *f == s.cleanup.filter)
            .unwrap_or(0);
        for (i, (_, name)) in LEVELS.iter().enumerate() {
            let r = ui.selectable_label(i == at, *name);
            if r.on_hover_text(format!("key {}", first_key(Action::CleanupFilter)))
                .clicked()
                && i != at
            {
                // The session steps through the levels in this order, round the end.
                let steps = (i + LEVELS.len() - at) % LEVELS.len();
                for _ in 0..steps {
                    out.push(Intent::Cleanup(Ask::CycleFilter));
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
        let native = cleanup::selected(s).is_some_and(|it| has_native(s, it));
        if ui
            .add_enabled(native, Button::new("Native command…"))
            .on_hover_text(format!("key {}", first_key(Action::CleanupCommand)))
            .clicked()
        {
            out.push(Intent::Act(Action::CleanupCommand));
        }
    });
}

/// The rule of `it` has a native command.
fn has_native(s: &SessionState, it: &Item) -> bool {
    (s.snap.rules.as_ref()).is_some_and(|r| r.rule(it.class.rule).action.native().is_some())
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
    ui.label(colors.text(parts.join(" · "), Token::FgDim));
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
    let row_h = list::row_height(ui);
    let mut table = TableBuilder::new(ui)
        .id_salt("dfdu-cleanup")
        .striped(true)
        .sense(Sense::click())
        .cell_layout(Layout::left_to_right(Align::Center))
        .column(Column::exact(row_h * 3.0))
        .column(Column::auto().at_least(70.0))
        .column(Column::auto().at_least(50.0))
        .column(Column::auto().at_least(80.0))
        .column(Column::auto().at_least(70.0))
        .column(Column::remainder())
        .min_scrolled_height(0.0)
        .auto_shrink(false);
    if std::mem::take(&mut view.follow) {
        table = table.scroll_to_row(cursor, None);
    }
    let now = now_secs();
    table
        .header(row_h, |mut h| {
            for label in ["", "Size", "Items", "Modified", "Why", "Path"] {
                h.col(|ui| {
                    ui.label(colors.text(label, Token::Header));
                });
            }
        })
        .body(|body| {
            body.rows(row_h, rows.len(), |mut row| {
                let i = row.index();
                row.set_selected(i == cursor);
                match rows[i] {
                    Row::Section(safety) => section(&mut row, s, colors, safety, i, out),
                    Row::Group { cat, safety } => group(&mut row, s, colors, cat, safety, i, out),
                    Row::Item(ix) => item(&mut row, s, colors, ix, i, now, out),
                }
                let r = row.response();
                if r.double_clicked() {
                    out.push(Intent::Cleanup(Ask::Select(i)));
                    out.push(Intent::Cleanup(Ask::Activate));
                } else if r.clicked() {
                    out.push(Intent::Cleanup(Ask::Select(i)));
                }
            });
        });
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

/// The first cell of a section or category on row `i`, with items `ix`: its triangle, which
/// opens or closes it, and its tick, which ticks every item in it; a category's indented.
fn opener(
    ui: &mut Ui,
    s: &SessionState,
    colors: &Colors,
    row: Row,
    ix: &[usize],
    i: usize,
    out: &mut Vec<Intent>,
) {
    let h = ui.text_style_height(&egui::TextStyle::Body);
    if matches!(row, Row::Group { .. }) {
        ui.add_space(0.8 * h);
    }
    let (rect, r) = ui.allocate_exact_size(vec2(h, h), Sense::click());
    let open = cleanup::is_open(s, row);
    let token = if r.hovered() { Token::Fg } else { Token::FgDim };
    let point = if open { Point::Down } else { Point::Right };
    list::triangle(ui, colors.fg(token), rect, point);
    let what = if open { "Collapse" } else { "Expand" };
    r.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, true, what));
    if r.clicked() {
        out.push(Intent::Cleanup(Ask::Select(i)));
        out.push(Intent::Cleanup(Ask::Activate));
    }
    if !s.read_only && review::tick(ui, colors, tick_of_items(s, ix)).clicked() {
        out.push(Intent::Cleanup(Ask::Select(i)));
        out.push(Intent::Cleanup(Ask::Toggle));
    }
}

/// The size and the count of items `ix`, in the size and items columns.
fn totals(row: &mut TableRow<'_, '_>, s: &SessionState, colors: &Colors, ix: &[usize]) {
    let size: u64 = ix.iter().map(|&i| s.extras.cleanup.items[i].size).sum();
    row.col(|ui| right(ui, colors.text(human(size), Token::Fg)));
    row.col(|ui| right(ui, colors.text(count(ix.len() as u64), Token::FgDim)));
}

/// `text` aligned to the right of its cell.
fn right(ui: &mut Ui, text: RichText) {
    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
        ui.label(text);
    });
}

/// A safety level's row: `Safe to delete · caches the owning program recreates`.
fn section(
    row: &mut TableRow<'_, '_>,
    s: &SessionState,
    colors: &Colors,
    safety: Safety,
    i: usize,
    out: &mut Vec<Intent>,
) {
    let ix = cleanup::section_items(s, safety);
    row.col(|ui| opener(ui, s, colors, Row::Section(safety), &ix, i, out));
    totals(row, s, colors, &ix);
    row.col(|_| {});
    row.col(|_| {});
    row.col(|ui| {
        let (name, meaning) = section_name(safety);
        ui.label(colors.text(name, Token::Title));
        ui.add(Label::new(colors.text(format!("· {meaning}"), Token::FgDim)).truncate());
    });
}

/// A category's row in its section, in the category's colour.
fn group(
    row: &mut TableRow<'_, '_>,
    s: &SessionState,
    colors: &Colors,
    cat: CategoryIx,
    safety: Safety,
    i: usize,
    out: &mut Vec<Intent>,
) {
    let ix = cleanup::group_items(s, cat, safety);
    let r = Row::Group { cat, safety };
    row.col(|ui| opener(ui, s, colors, r, &ix, i, out));
    totals(row, s, colors, &ix);
    row.col(|_| {});
    row.col(|_| {});
    row.col(|ui| {
        let category = cat.get();
        let token =
            (Category::ALL.get(category.color as usize)).map_or(Token::CatOther, |c| c.token());
        ui.label(colors.text(category.name, token));
    });
}

/// An item's row: its tick, size, entries, day modified, why it is suggested or not, and path.
fn item(
    row: &mut TableRow<'_, '_>,
    s: &SessionState,
    colors: &Colors,
    ix: usize,
    i: usize,
    now: i64,
    out: &mut Vec<Intent>,
) {
    let it = &s.extras.cleanup.items[ix];
    let marked = cleanup::is_marked(s, it);
    row.col(|ui| {
        let h = ui.text_style_height(&egui::TextStyle::Body);
        ui.add_space(2.6 * h);
        let tick = if !cleanup::markable(it) {
            Tick::Locked
        } else if marked {
            Tick::On
        } else {
            Tick::Off
        };
        if !s.read_only && review::tick(ui, colors, tick).clicked() {
            out.push(Intent::Cleanup(Ask::Select(i)));
            out.push(Intent::Cleanup(Ask::Toggle));
            out.push(Intent::Cleanup(Ask::Select(i)));
        }
    });
    let token = if marked { Token::Mark } else { Token::Fg };
    row.col(|ui| right(ui, colors.text(human(it.size), token)));
    row.col(|ui| right(ui, colors.text(count(it.items), Token::FgDim)));
    row.col(|ui| {
        if it.mtime > 0 {
            ui.label(colors.text(dfdu_tree::day(it.mtime), Token::FgDim));
        }
    });
    row.col(|ui| {
        let verdict = cleanup::recommended(s, it, now);
        let token = match verdict {
            Some(v) if v.recommended() => Token::StatusOk,
            Some(Verdict::InUse) => Token::StatusWarn,
            _ => Token::FgDim,
        };
        ui.label(colors.text(verdict.map_or("", chip), token));
    });
    row.col(|ui| {
        let path = tilde(&it.path, s.home.as_deref());
        // Not selectable, so a click on the path reaches the row.
        ui.add(
            Label::new(colors.text(path, Token::Fg))
                .selectable(false)
                .truncate(),
        );
    });
}

/// Below the table: the selected item's rule, what deleting it loses and why it is or is not
/// suggested. On a category, the rule of its first item; on a section, what its level means.
fn detail(ui: &mut Ui, s: &SessionState, colors: &Colors, row: Option<Row>) {
    let text = |ui: &mut Ui, t: RichText| {
        ui.add(Label::new(t).wrap());
    };
    let i = match row {
        Some(Row::Item(i)) => Some(i),
        Some(Row::Group { cat, safety }) => cleanup::group_items(s, cat, safety).first().copied(),
        Some(Row::Section(safety)) => {
            let (name, meaning) = section_name(safety);
            let n = cleanup::section_items(s, safety).len();
            text(ui, colors.text(name, Token::Title));
            text(
                ui,
                colors.text(
                    format!("{meaning}; {n} match{}", if n == 1 { "" } else { "es" }),
                    Token::FgDim,
                ),
            );
            return;
        }
        None => None,
    };
    let (Some(i), Some(rules)) = (i, &s.snap.rules) else {
        return;
    };
    let it = &s.extras.cleanup.items[i];
    let r = rules.rule(it.class.rule);
    ui.horizontal_wrapped(|ui| {
        ui.label(colors.text(&r.name, Token::Title));
        ui.label(colors.text(format!("· {}", it.class.safety.as_str()), Token::FgDim));
    });
    text(ui, RichText::new(&r.explanation));
    text(
        ui,
        colors.text(format!("You lose: {}", r.lost), Token::FgDim),
    );
    if !matches!(row, Some(Row::Item(_))) {
        return;
    }
    let warnings: Vec<&str> = [
        (ClassFlags::IN_USE, "in use"),
        (ClassFlags::VCS_TRACKED, "tracked by git"),
        (
            ClassFlags::VCS_UNKNOWN,
            "git could not tell if it is tracked",
        ),
    ]
    .into_iter()
    .filter(|&(flag, _)| it.class.has(flag))
    .map(|(_, what)| what)
    .collect();
    if !warnings.is_empty() {
        let w = format!("Warning: {}.", warnings.join(", "));
        text(ui, colors.text(w, Token::StatusWarn));
    }
    if let Some(v) = cleanup::recommended(s, it, now_secs()) {
        text(ui, colors.text(reason(v), Token::FgDim));
    }
    if r.action.native().is_some() {
        let key = first_key(Action::CleanupCommand);
        text(
            ui,
            colors.text(
                if s.read_only {
                    format!("{key} shows its own cleanup command and runs its dry run")
                } else {
                    format!("{key} runs its own cleanup command (dry run first)")
                },
                Token::FgDim,
            ),
        );
    }
}
