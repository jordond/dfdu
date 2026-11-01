//! The help (`?`, F1): the keys of the actions the window offers, from the action registry
//! grouped as the terminal's help groups them (`dfdu_session::actions::search`), with a search
//! that matches their keys and labels; then the platform's own shortcuts. In a read-only
//! browser the actions that delete are left out. `Esc` closes it.

use dfdu_session::actions::{Group, search};
use dfdu_session::palette::Token;
use egui::{Id, Label, ScrollArea, TextEdit, Window};

use crate::browser::ViewState;
use crate::keys;
use crate::theme::Colors;

/// The groups, in the order the help lists them, with their headings.
const GROUPS: [(Group, &str); 6] = [
    (Group::Navigate, "Navigate"),
    (Group::Sort, "Sort"),
    (Group::Display, "Display"),
    (Group::Act, "Act"),
    (Group::Panels, "Panels"),
    (Group::Global, "Global"),
];

/// The open help.
#[derive(Debug, Default)]
pub struct Help {
    pub query: String,
    /// The field gets the keyboard in the next frame.
    pub focus: bool,
}

impl Help {
    pub fn new() -> Self {
        Help {
            query: String::new(),
            focus: true,
        }
    }
}

/// The groups with the window's actions that match `query`, with each action's keys as shown
/// and its label.
pub fn rows(query: &str, read_only: bool) -> Vec<(&'static str, Vec<(String, &'static str)>)> {
    let groups = GROUPS.map(|(g, _)| g);
    search(query, read_only, &groups)
        .into_iter()
        .filter_map(|(g, found)| {
            let rows: Vec<(String, &str)> = (found.iter())
                .filter(|f| keys::OFFERED.contains(&f.info.action))
                .map(|f| (keys::shown(f.info.keys), f.info.label))
                .collect();
            let heading = GROUPS.iter().find(|(x, _)| *x == g).map(|(_, h)| *h)?;
            (!rows.is_empty()).then_some((heading, rows))
        })
        .collect()
}

/// The platform's shortcuts, with the name of its command key.
fn shortcuts(read_only: bool) -> Vec<(String, &'static str)> {
    let cmd = if cfg!(target_os = "macos") {
        "Cmd"
    } else {
        "Ctrl"
    };
    let mut out = vec![
        (format!("{cmd}-K"), "command palette"),
        (format!("{cmd}-F"), "filter this directory"),
        (format!("{cmd}-C"), "copy the selected path"),
    ];
    if !read_only {
        out.push(("Delete".into(), "move to Trash"));
    }
    out.push((format!("{cmd}-W"), "close the window"));
    out.push((format!("{cmd}-Q"), "quit"));
    out
}

/// Draws the help while it is open.
pub fn show(ctx: &egui::Context, read_only: bool, colors: &Colors, view: &mut ViewState) {
    let Some(help) = &mut view.help else { return };
    let mut open = true;
    let mut close = false;
    Window::new("Keys")
        .open(&mut open)
        .default_size([460.0, 520.0])
        .collapsible(false)
        .show(ctx, |ui| {
            let r = ui.add(
                TextEdit::singleline(&mut help.query)
                    .id(Id::unique("dfdu-help-search"))
                    .hint_text("Search the keys")
                    .desired_width(f32::INFINITY),
            );
            if std::mem::take(&mut help.focus) {
                r.request_focus();
            }
            if r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                close = true;
            }
            let rows = rows(&help.query, read_only);
            ScrollArea::vertical().auto_shrink(false).show(ui, |ui| {
                if rows.is_empty() {
                    ui.label(colors.text("Nothing matches.", Token::FgDim));
                }
                for (heading, found) in &rows {
                    ui.add_space(ui.spacing().item_spacing.y);
                    ui.label(colors.text(*heading, Token::Title).strong());
                    grid(ui, colors, heading, found);
                }
                if help.query.is_empty() {
                    ui.add_space(ui.spacing().item_spacing.y);
                    ui.label(colors.text("Shortcuts", Token::Title).strong());
                    let found: Vec<(String, &str)> = shortcuts(read_only);
                    grid(ui, colors, "shortcuts", &found);
                }
            });
        });
    if !open || close {
        view.help = None;
    }
}

/// The keys and labels of a group.
fn grid(ui: &mut egui::Ui, colors: &Colors, id: &str, rows: &[(String, &str)]) {
    egui::Grid::new(("dfdu-help", id)).show(ui, |ui| {
        for (keys, label) in rows {
            ui.label(colors.text(keys, Token::Key));
            ui.add(Label::new(*label).wrap());
            ui.end_row();
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_help_lists_the_window_s_keys_by_group_and_searches_them() {
        let all = rows("", false);
        let headings: Vec<&str> = all.iter().map(|(h, _)| *h).collect();
        assert_eq!(
            headings,
            ["Navigate", "Sort", "Display", "Act", "Panels", "Global"]
        );
        let act = &all[3].1;
        assert!(act.iter().any(|(k, l)| k == "d" && *l == "move to Trash"));
        // The arrows are named: the window's font has no arrow glyphs.
        assert!(all[0].1.iter().any(|(k, _)| k == "Up k"));
        // What the window does not offer is left out: quitting, the list's columns.
        assert!(!all.iter().flat_map(|(_, r)| r).any(|(_, l)| *l == "quit"));
        let read_only = rows("", true);
        assert!(!read_only[3].1.iter().any(|(k, _)| k == "d"));
        let found = rows("trash", false);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].0, "Act");
    }
}
