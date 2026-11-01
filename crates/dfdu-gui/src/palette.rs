//! The command palette (Cmd-K or Ctrl-K, and the registry's `:` and `Ctrl-P`): every action the
//! window offers and every theme, searched by name as the terminal's palette searches them
//! (`dfdu_session::fuzzy::rank`), each action with its keys, so the palette also teaches them.
//! `↑` and `↓` move, `Enter` (or a click) closes it and runs the entry, `Esc` closes it. In a
//! read-only browser the actions that delete are left out.

use dfdu_session::actions::{ACTIONS, Action, ActionInfo, Group};
use dfdu_session::palette::Token;
use egui::{Id, Key, Label, Modal, Modifiers, ScrollArea, TextEdit, Ui};

use crate::browser::{Intent, ViewState};
use crate::keys;
use crate::theme::Colors;

/// The id of the search field.
const FIELD: &str = "dfdu-palette";
/// Entries shown at once; the others scroll.
const ROWS: f32 = 12.0;

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
    let modal = Modal::new(Id::unique("dfdu-palette-modal")).show(ctx, |ui| {
        ui.set_width(520.0);
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
        let field = TextEdit::singleline(&mut p.query)
            .id(Id::unique(FIELD))
            .hint_text("Type a command or a theme")
            .desired_width(f32::INFINITY);
        let r = ui.add(field);
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
        ui.label(colors.text(
            format!("{} of {} commands", shown.len(), entries.len()),
            Token::FgDim,
        ));
        if shown.is_empty() {
            ui.label(colors.text("Nothing matches.", Token::FgDim));
        }
        let row_h = ui.text_style_height(&egui::TextStyle::Body) + ui.spacing().item_spacing.y;
        let mut run = enter.then_some(p.cursor);
        ScrollArea::vertical()
            .max_height(row_h * ROWS)
            .auto_shrink([false, true])
            .show(ui, |ui| {
                for (at, &i) in shown.iter().enumerate() {
                    if entry(ui, colors, &entries[i], at == p.cursor).clicked() {
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

/// One entry: its label on the left, its group and keys on the right; selected when `current`.
fn entry(ui: &mut Ui, colors: &Colors, e: &Entry, current: bool) -> egui::Response {
    let (group, keys) = match e {
        Entry::Action(info) => (group_name(info.group), keys::shown(info.keys)),
        Entry::Theme(..) => ("setting", String::new()),
    };
    ui.horizontal(|ui| {
        let r = ui.add(
            egui::Button::selectable(current, e.label())
                .frame_when_inactive(false)
                .min_size(egui::vec2(ui.available_width() * 0.7, 0.0)),
        );
        if current {
            r.scroll_to_me(None);
        }
        ui.add(Label::new(colors.text(group, Token::FgDim)).selectable(false));
        ui.add(Label::new(colors.text(keys, Token::Key)).selectable(false));
        r
    })
    .inner
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
