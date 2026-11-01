//! The bars below the list. The action bar, left out of a read-only browser: what is marked and
//! its total, what the last delete to the Trash or staging can put back (a staged delete with
//! the time left to do it), and the buttons that delete. The status bar: the status line of the
//! last action, what the engine is deleting or noticed, or what is selected in the view shown
//! ([`selection`]), on the left; the
//! scan's totals and what makes them partial on the right, its errors a link to their list.

use std::time::Instant;

use dfdu_engine::{Mode, STAGE_UNDO_WINDOW, tilde};
use dfdu_session::actions::Action;
use dfdu_session::format::{count, human, plural};
use dfdu_session::palette::Token;
use dfdu_session::view::View;
use dfdu_session::{SessionState, cleanup, dupes, largest};
use dfdu_tree::Kind;
use egui::{Align, Button, Layout, Ui};

use crate::browser::{Intent, ViewState};
use crate::theme::Colors;
use crate::{cleanup as cleanup_view, dupes as dupes_view};

pub fn show(
    ui: &mut Ui,
    s: &SessionState,
    colors: &Colors,
    view: &ViewState,
    now: Instant,
    out: &mut Vec<Intent>,
) {
    if !s.read_only {
        ui.horizontal(|ui| actions(ui, s, colors, view, now, out));
        ui.separator();
    }
    ui.horizontal(|ui| {
        status(ui, s, colors);
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            totals(ui, s, colors, out);
        });
    });
}

/// The marks, the undo and the buttons that delete.
fn actions(
    ui: &mut Ui,
    s: &SessionState,
    colors: &Colors,
    view: &ViewState,
    now: Instant,
    out: &mut Vec<Intent>,
) {
    match marked(s) {
        Some(text) => {
            ui.label(colors.text(text, Token::Mark));
            if ui.button("Clear marks").clicked() {
                out.push(Intent::Act(Action::ClearMarks));
            }
        }
        None => {
            ui.label(colors.text("Nothing marked", Token::FgDim));
        }
    }
    if let Some(text) = undo_note(s, view, now) {
        ui.separator();
        ui.label(colors.text(text, Token::StatusWarn));
        let button = ui.add_enabled(!s.snap.undoing, Button::new("Undo"));
        if button.clicked() {
            out.push(Intent::Act(Action::Undo));
        }
    }
    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
        let target = !s.marks.is_empty() || s.selected_entry().is_some_and(|e| !e.pseudo);
        if ui
            .add_enabled(target, Button::new("Delete permanently…"))
            .clicked()
        {
            out.push(Intent::Act(Action::DeletePermanent));
        }
        if ui
            .add_enabled(target, Button::new("Move to Trash…"))
            .clicked()
        {
            out.push(Intent::Act(Action::Trash));
        }
    });
}

/// How many entries are marked and their total; `None` when nothing is.
pub fn marked(s: &SessionState) -> Option<String> {
    let marks = &s.marks;
    if marks.is_empty() {
        return None;
    }
    let (total, scanning) = marks.total();
    let at_least = if scanning { "at least " } else { "" };
    Some(format!(
        "{} marked ({at_least}{})",
        marks.items().len(),
        human(total)
    ))
}

/// What the undo puts back, and for a staged delete how long it still can: the engine deletes
/// what it staged [`STAGE_UNDO_WINDOW`] after staging it.
pub fn undo_note(s: &SessionState, view: &ViewState, now: Instant) -> Option<String> {
    let u = s.snap.undo.as_ref()?;
    let what = format!(
        "{} item{} ({})",
        u.items,
        plural(u.items as u64),
        human(u.bytes)
    );
    Some(match u.mode {
        Mode::Stage => {
            let seen = view
                .undo_seen
                .filter(|(plan, _)| *plan == u.plan)
                .map_or(now, |(_, at)| at);
            let left = STAGE_UNDO_WINDOW.saturating_sub(now.duration_since(seen));
            format!("Staged {what}: deleted in {} s", left.as_secs())
        }
        Mode::Trash => format!("Moved {what} to the Trash"),
        Mode::Permanent => format!("Deleted {what}"),
    })
}

/// The status line: a delete or an undo running, else the last action's message or the
/// engine's last notice, else what is selected in the view shown.
fn status(ui: &mut Ui, s: &SessionState, colors: &Colors) {
    let msg = match s.snap.deleting {
        Some((done, total)) => Some(format!("deleting {done}/{total}…")),
        None if s.snap.undoing => Some("putting back…".into()),
        None => s.status.clone().or_else(|| s.snap.notice.clone()),
    };
    if let Some(msg) = msg {
        ui.label(colors.text(msg, Token::StatusWarn));
        return;
    }
    if let Some(sel) = selection(s) {
        ui.label(colors.text(sel.name, Token::Fg));
        if let Some(size) = sel.size {
            ui.label(colors.text(human(size), Token::SizeBig));
        }
        if let Some(note) = sel.note {
            ui.label(colors.text(note, Token::FgDim));
        }
    }
}

/// What the status bar says of the selection.
#[derive(Debug, PartialEq)]
pub struct Selection {
    pub name: String,
    pub size: Option<u64>,
    pub note: Option<String>,
}

/// What is selected in the view shown: the list's entry (the map's too), the largest file, the
/// cleanup row or the duplicates row under the cursor; `None` when nothing is.
pub fn selection(s: &SessionState) -> Option<Selection> {
    match s.current_view() {
        View::Files | View::Map => {
            let e = s.selected_entry()?;
            Some(Selection {
                name: String::from_utf8_lossy(&e.name).into_owned(),
                size: Some(e.size),
                note: e.is_dir.then(|| items(e.items)),
            })
        }
        View::Largest => {
            let (_, f) = largest::selected(s)?;
            Some(Selection {
                name: f.path.display().to_string(),
                size: Some(f.size),
                note: None,
            })
        }
        View::Cleanup => cleanup_selection(s),
        View::Duplicates => dupes_selection(s),
    }
}

/// `3 items`.
fn items(n: u64) -> String {
    format!("{} item{}", count(n), plural(n))
}

/// `2 matches`.
fn matches(n: usize) -> String {
    format!("{n} match{}", if n == 1 { "" } else { "es" })
}

/// The cleanup row under the cursor: an item by its path, a category or a level by its name,
/// with what its items add up to.
fn cleanup_selection(s: &SessionState) -> Option<Selection> {
    let all = &s.extras.cleanup.items;
    let total = |ix: &[usize]| ix.iter().map(|&i| all[i].size).sum();
    match *cleanup::rows(s).get(s.cleanup.cursor)? {
        cleanup::Row::Item(i) => {
            let it = all.get(i)?;
            Some(Selection {
                name: tilde(&it.path, s.home.as_deref()),
                size: Some(it.size),
                note: (it.kind == Kind::Dir).then(|| items(it.items)),
            })
        }
        cleanup::Row::Group { cat, safety } => {
            let ix = cleanup::group_items(s, cat, safety);
            Some(Selection {
                name: cat.get().name.to_owned(),
                size: Some(total(&ix)),
                note: Some(matches(ix.len())),
            })
        }
        cleanup::Row::Section(safety) => {
            let ix = cleanup::section_items(s, safety);
            Some(Selection {
                name: cleanup_view::section_name(safety).0.to_owned(),
                size: Some(total(&ix)),
                note: Some(matches(ix.len())),
            })
        }
    }
}

/// The duplicates row under the cursor: a copy by its path below the scan root, a group by how
/// many copies it has and what deleting them frees; nothing before a comparison finished.
fn dupes_selection(s: &SessionState) -> Option<Selection> {
    let found = dupes::found(s)?;
    let root = &s.dupes.run.as_ref()?.root;
    match *dupes::rows(s).get(s.dupes.cursor)? {
        dupes::Row::File(g, f) => {
            let c = &found.groups.get(g)?.files.get(f)?;
            Some(Selection {
                name: dupes_view::relative_to_root(&c.path, root)
                    .display()
                    .to_string(),
                size: Some(c.size),
                note: None,
            })
        }
        dupes::Row::Group(g) => {
            let group = found.groups.get(g)?;
            let n = group
                .files
                .iter()
                .filter(|c| dupes::live(&s.extras.dupes, c).is_some())
                .count();
            Some(Selection {
                name: format!("{n} copies"),
                size: Some(group.size),
                note: Some(if group.all_clones() {
                    "clones: frees nothing".to_owned()
                } else {
                    format!("frees {}", human(group.wasted()))
                }),
            })
        }
    }
}

/// The scan's totals, right to left: its size and items, then errors (a click lists them), the
/// filter of file types and an incomplete scan.
fn totals(ui: &mut Ui, s: &SessionState, colors: &Colors, out: &mut Vec<Intent>) {
    let snap = &s.snap;
    let root = snap.root_total;
    ui.label(colors.text(human(s.size_mode.of(&root)), Token::Header));
    ui.label(colors.text(
        format!("{} item{}", count(root.items), plural(root.items)),
        Token::FgDim,
    ));
    if root.errors > 0 {
        let text = format!("{} error{}", count(root.errors), plural(root.errors));
        if ui.link(colors.text(text, Token::StatusError)).clicked() {
            out.push(Intent::Act(Action::Errors));
        }
    }
    if snap.incomplete {
        ui.label(colors.text("incomplete", Token::StatusWarn));
    }
    if snap.stalled > 0 {
        ui.label(colors.text(
            format!(
                "{} director{} not responding",
                snap.stalled,
                if snap.stalled == 1 { "y" } else { "ies" }
            ),
            Token::StatusWarn,
        ));
    }
}
