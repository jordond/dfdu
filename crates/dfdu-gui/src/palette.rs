//! The command palette (Cmd-K or Ctrl-K, and the registry's `:` and `Ctrl-P`): every action the
//! window offers and every theme, searched by name as the terminal's palette searches them
//! (`dfdu_session::fuzzy::rank`), each action with its keys, so the palette also teaches them.
//! `↑` and `↓` move, `Enter` (or a click) closes it and runs the entry, `Esc` closes it. In a
//! read-only browser the actions that delete are left out. It floats near the top on `raised`
//! with the menus' shadow, 560 wide: the search, then the entries under their groups' headings
//! (ranked, with the group beside them, once something is typed), each key in a cap.

use dfdu_session::actions::{ACTIONS, Action, ActionInfo, Group};
use dfdu_session::palette::Token;
use egui::{
    Align, Align2, Color32, CornerRadius, Frame, Id, Key, Label, Layout, Margin, Modal, Modifiers,
    Response, ScrollArea, Sense, Ui, UiBuilder, WidgetInfo, vec2,
};

use crate::browser::{Intent, ViewState};
use crate::theme::Colors;
use crate::theme::style::{RADIUS_SM, ROW, SPACE_1, SPACE_2};
use crate::{keys, kit};

/// The id of the search field.
const FIELD: &str = "dfdu-palette";
/// Entries shown at once; the others scroll.
const ROWS: f32 = 14.0;
/// The palette's width.
const WIDTH: f32 = 560.0;

/// The open palette.
#[derive(Debug, Default)]
pub struct Palette {
    pub query: String,
    /// Index into the matches.
    pub cursor: usize,
    /// The field gets the keyboard in the next frame.
    pub focus: bool,
}

impl Palette {
    pub fn new() -> Self {
        Palette {
            focus: true,
            ..Palette::default()
        }
    }
}

/// An action's row of the registry: what the palette shows of it.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct Row {
    pub action: Action,
    pub keys: &'static str,
    pub label: &'static str,
    pub group: Group,
}

impl From<&'static ActionInfo> for Row {
    fn from(info: &'static ActionInfo) -> Self {
        Row {
            action: info.action,
            keys: info.keys,
            label: info.label,
            group: info.group,
        }
    }
}

/// An entry of the palette.
#[derive(Clone, Debug, PartialEq)]
pub enum Entry {
    /// An action of the registry, by its row.
    Action(Row),
    /// Theme `i` of the window's themes, by its name.
    Theme(usize, String),
}

impl Entry {
    /// What the search matches.
    pub fn label(&self) -> String {
        match self {
            Entry::Action(info) => info.label.to_owned(),
            Entry::Theme(_, name) => format!("Theme: {name}"),
        }
    }

    /// The group of an action; `None` for a theme.
    fn group(&self) -> Option<Group> {
        match self {
            Entry::Action(info) => Some(info.group),
            Entry::Theme(..) => None,
        }
    }

    /// What running it asks of the window.
    fn intent(&self) -> Intent {
        match self {
            Entry::Action(info) => Intent::Act(info.action),
            Entry::Theme(i, _) => Intent::Theme(*i),
        }
    }
}

/// Everything the palette offers: the actions the window offers, less the palette itself and,
/// when `read_only`, those that delete; then the themes named `themes`.
pub fn entries(read_only: bool, themes: &[String]) -> Vec<Entry> {
    let actions = (ACTIONS.iter())
        .filter(|info| info.action != Action::Palette && keys::OFFERED.contains(&info.action))
        .filter(|info| !(read_only && info.action.deletes()))
        .map(|info| Entry::Action(info.into()));
    let themes = (themes.iter().enumerate()).map(|(i, name)| Entry::Theme(i, name.clone()));
    actions.chain(themes).collect()
}

/// The entries `query` matches, best first.
pub fn matches(entries: &[Entry], query: &str) -> Vec<usize> {
    let labels: Vec<String> = entries.iter().map(Entry::label).collect();
    dfdu_session::fuzzy::rank(query, labels.iter().map(String::as_str))
        .into_iter()
        .map(|(i, _)| i)
        .collect()
}

/// The name of a group, as the help shows it.
pub fn group_name(g: Group) -> &'static str {
    match g {
        Group::Navigate => "navigate",
        Group::Sort => "sort",
        Group::Display => "display",
        Group::Act => "act",
        Group::Panels => "panels",
        Group::Global => "global",
    }
}

/// Draws the palette while it is open.
pub fn show(
    ctx: &egui::Context,
    read_only: bool,
    colors: &Colors,
    view: &mut ViewState,
    out: &mut Vec<Intent>,
) {
    let entries = entries(read_only, &view.themes);
    let Some(p) = &mut view.palette else { return };
    let mut close = false;
    let style = ctx.global_style();
    let frame = Frame::window(&style)
        .shadow(style.visuals.popup_shadow)
        .inner_margin(Margin::same(12));
    let id = Id::unique("dfdu-palette-modal");
    let modal = Modal::new(id)
        .area(Modal::default_area(id).anchor(Align2::CENTER_TOP, vec2(0.0, 72.0)))
        .backdrop_color(Color32::TRANSPARENT)
        .frame(frame)
        .show(ctx, |ui| {
            ui.set_width(WIDTH - 24.0);
            let shown = matches(&entries, &p.query);
            let last = shown.len().saturating_sub(1);
            // The arrows move among the entries, not in the field.
            let (down, up) = ui.input_mut(|i| {
                (
                    i.consume_key(Modifiers::NONE, Key::ArrowDown),
                    i.consume_key(Modifiers::NONE, Key::ArrowUp),
                )
            });
            if down {
                p.cursor = (p.cursor + 1).min(last);
            }
            if up {
                p.cursor = p.cursor.saturating_sub(1);
            }
            let width = ui.available_width();
            let hint = "Type a command or a theme";
            let r = crate::windows::field(ui, colors, &mut p.query, Id::unique(FIELD), hint, width);
            if std::mem::take(&mut p.focus) {
                r.request_focus();
            }
            if r.changed() {
                p.cursor = 0;
            }
            let enter = r.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter));
            if r.lost_focus() && ui.input(|i| i.key_pressed(Key::Escape)) {
                close = true;
            }
            let dim = colors.fg(Token::FgDim);
            ui.add_space(SPACE_1);
            let counted = format!("{} of {} commands", shown.len(), entries.len());
            ui.label(kit::mono(counted, 11.0).color(dim));
            if shown.is_empty() {
                ui.label(kit::sans("Nothing matches.", 12.0).color(dim));
            }
            let mut run = enter.then_some(p.cursor);
            // Grouped under headings until something is typed; ranked after.
            let grouped = p.query.trim().is_empty();
            ScrollArea::vertical()
                .max_height(ROW * ROWS)
                .auto_shrink([false, true])
                .show(ui, |ui| {
                    ui.spacing_mut().item_spacing.y = 0.0;
                    let mut last = None;
                    for (at, &i) in shown.iter().enumerate() {
                        let group = entries[i].group();
                        if grouped && last != Some(group) {
                            ui.add_space(if last.is_some() { SPACE_2 } else { SPACE_1 });
                            ui.label(kit::label(heading(group), 11.0).color(dim));
                            ui.add_space(SPACE_1);
                            last = Some(group);
                        }
                        if entry(ui, colors, &entries[i], at == p.cursor, !grouped).clicked() {
                            run = Some(at);
                        }
                    }
                });
            if let Some(at) = run {
                close = true;
                if let Some(&i) = shown.get(at) {
                    out.push(entries[i].intent());
                }
            }
        });
    if close || modal.should_close() {
        view.palette = None;
    }
}

/// The heading of the entries of `group`; the themes' when `None`.
fn heading(group: Option<Group>) -> &'static str {
    match group {
        Some(Group::Navigate) => "Navigate",
        Some(Group::Sort) => "Sort",
        Some(Group::Display) => "Display",
        Some(Group::Act) => "Act",
        Some(Group::Panels) => "Panels",
        Some(Group::Global) => "Global",
        None => "Themes",
    }
}

/// One entry, a row 24 high: its label on the left, its keys in caps on the right, and its
/// group before them when `with_group` (the entries ranked, not under headings); in the
/// cursor's colours when `current`.
fn entry(ui: &mut Ui, colors: &Colors, e: &Entry, current: bool, with_group: bool) -> Response {
    let (group, keys) = match e {
        Entry::Action(info) => (group_name(info.group), keys::shown(info.keys)),
        Entry::Theme(..) => ("setting", String::new()),
    };
    let label = e.label();
    let (rect, r) = ui.allocate_exact_size(vec2(ui.available_width(), ROW), Sense::click());
    r.widget_info(|| WidgetInfo::selected(egui::Role::Button, true, current, &label));
    let c = &colors.chrome;
    let radius = CornerRadius::same(RADIUS_SM);
    let fg = if current {
        ui.painter()
            .rect_filled(rect, radius, colors.bg(Token::Cursor).unwrap_or(c.hover));
        colors.fg(Token::Cursor)
    } else {
        if r.hovered() {
            ui.painter().rect_filled(rect, radius, c.hover);
        }
        colors.fg(Token::Fg)
    };
    if current {
        r.scroll_to_me(None);
    }
    let mut row = ui.new_child(
        UiBuilder::new()
            .max_rect(rect.shrink2(vec2(SPACE_2, 0.0)))
            .layout(Layout::right_to_left(Align::Center)),
    );
    row.spacing_mut().item_spacing.x = SPACE_2;
    crate::help::caps(&mut row, colors, &keys);
    if with_group {
        let group = kit::label(group, 11.0).color(colors.fg(Token::FgDim));
        row.add(Label::new(group).selectable(false));
    }
    row.with_layout(Layout::left_to_right(Align::Center), |ui| {
        ui.add(
            Label::new(kit::sans(label.as_str(), 13.0).color(fg))
                .selectable(false)
                .truncate(),
        );
    });
    r
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_palette_offers_the_window_s_actions_and_the_themes() {
        let themes = ["system".to_owned(), "dark".to_owned()];
        let all = entries(false, &themes);
        let has = |es: &[Entry], a: Action| {
            es.iter()
                .any(|e| matches!(e, Entry::Action(i) if i.action == a))
        };
        assert!(has(&all, Action::Trash) && has(&all, Action::Help));
        // Not itself, nor what the window does not offer.
        assert!(!has(&all, Action::Palette) && !has(&all, Action::Quit));
        assert!(all.contains(&Entry::Theme(1, "dark".into())));
        let read_only = entries(true, &themes);
        assert!(!has(&read_only, Action::Trash) && !has(&read_only, Action::Undo));

        // The search ranks by score; the theme's name finds it.
        let found = matches(&all, "theme dark");
        assert_eq!(all[found[0]], Entry::Theme(1, "dark".into()));
        let found = matches(&all, "trash");
        assert!(matches!(all[found[0]], Entry::Action(i) if i.action == Action::Trash));
        assert_eq!(matches(&all, "").len(), all.len());
    }
}
