//! Below the Cleanup table: the selected item's rule, what deleting it loses and why it is or is
//! not suggested, with the button that opens its native command. On a category, the rule of
//! its first item; on a section, what its level means.

use dfdu_engine::Verdict;
use dfdu_session::SessionState;
use dfdu_session::actions::{Action, first_key};
use dfdu_session::cleanup::{self, Row};
use dfdu_session::format::human;
use dfdu_session::palette::Token;
use dfdu_tree::{ClassFlags, now_secs};
use egui::{Label, RichText, Ui};

use crate::browser::Intent;
use crate::kit::{self, Kind};
use crate::theme::Colors;

use super::section_name;

/// Why an item is or is not suggested, as a sentence.
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

/// Draws the detail of `row`.
pub fn show(
    ui: &mut Ui,
    s: &SessionState,
    colors: &Colors,
    row: Option<Row>,
    out: &mut Vec<Intent>,
) {
    let dim = colors.fg(Token::FgDim);
    let text = |ui: &mut Ui, t: RichText| {
        ui.add(Label::new(t).wrap());
    };
    let i = match row {
        Some(Row::Item(i)) => Some(i),
        Some(Row::Group { cat, safety }) => cleanup::group_items(s, cat, safety).first().copied(),
        Some(Row::Section(safety)) => {
            let (name, meaning) = section_name(safety);
            let n = cleanup::section_items(s, safety).len();
            let (glyph, token) = crate::details::safety(safety);
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 6.0;
                ui.label(kit::title(glyph, 14.0).color(colors.fg(token)));
                ui.label(kit::title(name, 14.0).color(colors.fg(token)));
            });
            let line = format!("{meaning}; {n} match{}", if n == 1 { "" } else { "es" });
            text(ui, kit::sans(line, 12.0).color(dim));
            return;
        }
        None => None,
    };
    let (Some(i), Some(rules)) = (i, &s.snap.rules) else {
        return;
    };
    let it = &s.extras.cleanup.items[i];
    let r = rules.rule(it.class.rule);
    let native = r.action.native().is_some();
    ui.horizontal(|ui| {
        ui.label(kit::title(&r.name, 14.0).color(colors.fg(Token::Title)));
        let (glyph, token) = crate::details::safety(it.class.safety);
        let level = format!("{glyph} {}", it.class.safety.as_str());
        ui.label(kit::label(level, 12.0).color(colors.fg(token)));
        if native && matches!(row, Some(Row::Item(_))) {
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let key = first_key(Action::CleanupCommand);
                if kit::button(
                    ui,
                    colors,
                    Kind::Secondary,
                    "Native command…",
                    Some(key),
                    true,
                )
                .clicked()
                {
                    out.push(Intent::Act(Action::CleanupCommand));
                }
            });
        }
    });
    text(
        ui,
        kit::sans(&r.explanation, 13.0).color(colors.fg(Token::Fg)),
    );
    text(
        ui,
        kit::sans(format!("You lose: {}", r.lost), 12.0).color(dim),
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
        text(ui, kit::sans(w, 12.0).color(colors.fg(Token::StatusWarn)));
    }
    if let Some(v) = cleanup::recommended(s, it, now_secs()) {
        text(ui, kit::sans(reason(v), 12.0).color(dim));
    }
    if native {
        let key = first_key(Action::CleanupCommand);
        let what = if s.read_only {
            format!("{key} shows its own cleanup command and runs its dry run")
        } else {
            format!("{key} runs its own cleanup command (dry run first)")
        };
        text(ui, kit::sans(what, 12.0).color(dim));
    }
}
