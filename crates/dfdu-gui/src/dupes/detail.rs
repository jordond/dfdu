//! Below the Duplicates table: the selected copy and what deleting it frees, then which copy
//! of its group is kept and which go.

use std::path::Path;

use dfdu_dupes::{Candidate, Found};
use dfdu_session::SessionState;
use dfdu_session::dupes::{self, Row};
use dfdu_session::format::{human, plural};
use dfdu_session::palette::Token;
use dfdu_tree::HostPath;
use egui::{Label, Ui};

use super::relative_to_root;
use crate::kit;
use crate::theme::Colors;

/// Copies of a group listed before "and N more".
const SHOWN: usize = 6;

/// What deleting copy `c` frees, in words.
fn frees(c: &Candidate) -> String {
    if c.links > 0 {
        format!(
            "has {} other name{} (hard links): deleting this one frees nothing",
            c.links,
            plural(u64::from(c.links))
        )
    } else {
        match c.private {
            Some(0) => "a clone: shares all its data, deleting it frees nothing".into(),
            Some(p) if p < c.alloc => format!("shares some data: deleting it frees {}", human(p)),
            _ => format!("deleting it frees {}", human(c.freed())),
        }
    }
}

/// Draws the detail of `row`.
pub fn show(
    ui: &mut Ui,
    s: &SessionState,
    colors: &Colors,
    found: &Found,
    row: Option<&Row>,
    root: &Path,
) {
    let Some(&row) = row else { return };
    let (Row::Group(g) | Row::File(g, _)) = row;
    let group = &found.groups[g];
    let nodes = &s.extras.dupes;
    let dim = colors.fg(Token::FgDim);
    if let Row::File(_, i) = row {
        let c = &group.files[i];
        let name = dfdu_engine::tilde(&HostPath::local(&c.path), s.home.as_deref());
        let name = kit::mono_bold(name, 13.0).color(colors.fg(Token::Title));
        ui.add(Label::new(name).truncate());
        let modified = dfdu_tree::date_time(c.mtime);
        let line = format!("{} · modified {modified} · {}", human(c.size), frees(c));
        ui.label(kit::sans(line, 12.0).color(dim));
    }
    let keep = dupes::kept(group, s.dupes.keep, nodes);
    let live: Vec<(usize, &Candidate)> = (group.files.iter().enumerate())
        .filter(|(_, c)| dupes::live(nodes, c).is_some())
        .collect();
    for &(i, c) in live.iter().take(SHOWN) {
        let rel = relative_to_root(&c.path, root).display().to_string();
        ui.horizontal(|ui| {
            let (word, token) = if Some(i) == keep {
                ("kept", Token::StatusOk)
            } else {
                ("goes", Token::FgDim)
            };
            ui.add_sized(
                [36.0, 16.0],
                Label::new(kit::label(word, 12.0).color(colors.fg(token))),
            );
            let rel = kit::mono(rel, 12.0).color(colors.fg(Token::Fg));
            ui.add(Label::new(rel).truncate());
        });
    }
    if live.len() > SHOWN {
        let more = format!("and {} more", live.len() - SHOWN);
        ui.label(kit::sans(more, 12.0).color(dim));
    }
}
