//! The delete confirmation, for the marked entries or else the selected one. It lists the
//! targets, their total and the method: `y` moves them to the Trash, `p` switches between the
//! Trash and a permanent delete, `s` between the Trash and staging (renamed aside at once, put
//! back with `U` for a minute, then deleted). A permanent delete, and a move to the Trash of 50 GiB or more
//! or of a directory still being scanned, needs the word "delete" typed and `Enter`.

use std::cmp::Reverse;

use dfdu_engine::{Command, Mode};
use dfdu_tree::NodeRef;
use ratatui::Frame;
use ratatui::crossterm::event::{KeyCode, KeyEvent};
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::{Line, Span};

use crate::app::{App, Effect, Overlay};
use crate::format::{count, human};
use crate::input::line::{Edit, LineInput};
use crate::input::line_keys;
use crate::model::outside::rel_text;
use crate::model::snapshot::{EntryData, Mark};
use crate::theme::Theme;
use crate::view::{cells, input_spans};

/// The word a typed confirmation asks for.
const WORD: &str = "delete";
/// Longest text the confirmation input takes.
const TYPED_MAX: usize = 16;
/// A move to the Trash of this much or more also needs the word typed.
const LARGE: u64 = 50 << 30;
/// Targets listed before "and N more".
const SHOWN: usize = 8;

/// One entry the dialog lists.
#[derive(Clone, Debug)]
pub struct Item {
    pub node: NodeRef,
    /// The name, or the path relative to the current directory (the full path when it is not
    /// below it).
    pub name: String,
    pub size: u64,
    pub items: u64,
    /// Part of `size` that data outside the scan also uses.
    pub shared_out: u64,
    pub is_dir: bool,
    /// A directory still being scanned: `size` is a lower bound.
    pub scanning: bool,
    /// Below another target: deleted with it and not counted again.
    pub inside: bool,
}

impl Item {
    fn of(e: &EntryData) -> Self {
        Item {
            node: e.node,
            name: String::from_utf8_lossy(&e.name).into_owned(),
            size: e.size,
            items: e.items,
            shared_out: e.shared_out,
            is_dir: e.is_dir,
            scanning: matches!(e.mark, Mark::Scanning | Mark::Stalled | Mark::Rescanning),
            inside: false,
        }
    }

    fn label(&self) -> String {
        if self.is_dir {
            format!("{}/", self.name)
        } else {
            self.name.clone()
        }
    }
}

/// The open dialog.
#[derive(Clone, Debug)]
pub struct Confirm {
    /// The targets sent to the engine, largest first, then those inside them.
    targets: Vec<Item>,
    /// Size, entries and size used outside the scan of the targets sent.
    total: u64,
    items: u64,
    shared_out: u64,
    mode: Mode,
    typed: LineInput,
    /// The word must be typed to confirm.
    typing: bool,
    /// The id of the engine's estimate of what the delete frees ([`Command::Estimate`]).
    estimate: u64,
}

impl Confirm {
    pub fn new(mut targets: Vec<Item>, mode: Mode) -> Self {
        targets.sort_by_key(|t| (t.inside, Reverse(t.size)));
        let sent = targets.iter().filter(|t| !t.inside);
        let (total, items, shared_out) = sent.fold((0, 0, 0), |(n, i, s), t| {
            (n + t.size, i + t.items, s + t.shared_out)
        });
        let mut c = Confirm {
            targets,
            total,
            items,
            shared_out,
            mode,
            typed: LineInput::default(),
            typing: false,
            estimate: 0,
        };
        c.set_mode(mode);
        c
    }

    fn set_mode(&mut self, mode: Mode) {
        self.mode = mode;
        self.typing = mode == Mode::Permanent || self.total >= LARGE || self.scanning();
        self.typed.clear();
    }

    /// A target sent is still being scanned: `total` is a lower bound.
    fn scanning(&self) -> bool {
        self.sent().any(|t| t.scanning)
    }

    fn sent(&self) -> impl Iterator<Item = &Item> {
        self.targets.iter().filter(|t| !t.inside)
    }
}

/// Asks to delete the marked entries, the outermost of them, or without marks the selected
/// entry, and asks the engine what that would free. The "shared between entries" row and
/// deleted rows cannot be deleted.
pub fn open(app: &mut App, mode: Mode) -> Vec<Effect> {
    let targets: Vec<Item> = if app.marks.is_empty() {
        let e = app.selected_entry().filter(|e| !e.pseudo);
        e.map(Item::of).into_iter().collect()
    } else {
        marked(app, |_| true)
    };
    ask(app, mode, targets)
}

/// Asks to delete the marked entries among `only` (a review view's): the other marks stay.
pub fn open_only(
    app: &mut App,
    mode: Mode,
    only: &std::collections::HashSet<dfdu_tree::NodeRef>,
) -> Vec<Effect> {
    let targets = marked(app, |n| only.contains(&n));
    ask(app, mode, targets)
}

/// The marked entries whose node `keep` lets in, named below the current directory.
/// A mark inside another counts as inside only while that other one is kept too: left out,
/// it is deleted on its own.
fn marked(app: &App, keep: impl Fn(dfdu_tree::NodeRef) -> bool) -> Vec<Item> {
    let dir = &app.snap.dir_path;
    let kept: Vec<&crate::state::marks::Item> = (app.marks.items().iter())
        .filter(|m| m.target.node.is_some_and(&keep))
        .collect();
    let all = kept.len() == app.marks.items().len();
    let below_a_kept = |m: &crate::state::marks::Item| {
        kept.iter().any(|o| {
            o.target.node != m.target.node
                && m.target
                    .path
                    .rel(&o.target.path)
                    .is_some_and(|r| !r.is_empty())
        })
    };
    kept.iter()
        .filter_map(|m| {
            let path = &m.target.path;
            let shown = match rel_text(path, dir) {
                Some(rel) if !rel.is_empty() => rel,
                _ => path.to_string(),
            };
            Some(Item {
                node: m.target.node?,
                name: shown,
                size: m.size,
                items: m.items,
                shared_out: m.shared_out,
                is_dir: m.is_dir,
                scanning: m.scanning,
                inside: m.inside && (all || below_a_kept(m)),
            })
        })
        .collect()
}

/// Opens the dialog for `targets`, and asks the engine what deleting them would free.
fn ask(app: &mut App, mode: Mode, targets: Vec<Item>) -> Vec<Effect> {
    if targets.is_empty() {
        return Vec::new();
    }
    if app.marks.is_empty() && targets[0].is_dir && crate::overlay::type_filter::active(app) {
        app.status = Some(crate::overlay::type_filter::NO_FOLDERS.into());
        return Vec::new();
    }
    let mut c = Confirm::new(targets, mode);
    app.estimates += 1;
    c.estimate = app.estimates;
    let items = c.sent().map(|t| t.node).collect();
    app.overlay = Overlay::ConfirmDelete(c);
    vec![Effect::Send(Command::Estimate {
        id: app.estimates,
        items,
    })]
}

/// Handles a key while the dialog is open: confirms, switches the method, cancels or edits the
/// typed word. Confirming sends one delete of every target that is not inside another.
pub fn key(app: &mut App, k: KeyEvent) -> Vec<Effect> {
    let Overlay::ConfirmDelete(c) = &mut app.overlay else {
        return Vec::new();
    };
    let confirmed = match k.code {
        KeyCode::Esc | KeyCode::Char('n') => false,
        KeyCode::Char('p' | 'P') => {
            let other = match c.mode {
                Mode::Trash | Mode::Stage => Mode::Permanent,
                Mode::Permanent => Mode::Trash,
            };
            c.set_mode(other);
            return Vec::new();
        }
        KeyCode::Char('s' | 'S') if !c.typing || c.typed.text().is_empty() => {
            let other = match c.mode {
                Mode::Stage => Mode::Trash,
                _ => Mode::Stage,
            };
            c.set_mode(other);
            return Vec::new();
        }
        _ if !c.typing => matches!(k.code, KeyCode::Char('y' | 'Y')),
        KeyCode::Char(_) if c.typed.text().chars().count() >= TYPED_MAX => return Vec::new(),
        _ => match line_keys::edit(&mut c.typed, &k) {
            Edit::Submit => {
                if c.typed.text() != WORD {
                    app.status = Some("not deleted: type delete to confirm".into());
                }
                c.typed.text() == WORD
            }
            Edit::Cancel => false,
            Edit::Changed | Edit::Moved | Edit::Ignored => return Vec::new(),
        },
    };
    let mut fx = Vec::new();
    if confirmed {
        let mode = c.mode;
        fx.push(Effect::Send(Command::Delete {
            items: c.sent().map(|t| t.node).collect(),
            mode,
        }));
        app.status = Some(match mode {
            Mode::Trash => "moving to the Trash…".into(),
            Mode::Stage => "staging…".into(),
            Mode::Permanent => "deleting…".into(),
        });
    }
    app.overlay = Overlay::None;
    fx
}

/// Draws the dialog centred in `area` and returns the rectangle it covers.
pub fn draw(f: &mut Frame, app: &App, area: Rect) -> Rect {
    let Overlay::ConfirmDelete(c) = &app.overlay else {
        return Rect::default();
    };
    let t = &app.theme;
    let mut lines = question(c, t);
    targets(&mut lines, c, t);
    frees(&mut lines, app, c);
    lines.push(Line::raw(""));
    method(&mut lines, c, t);
    // Wide enough for the longest line (a long name, the "may free less" note), at least 60.
    let width = lines
        .iter()
        .map(|l| l.width() + 3)
        .max()
        .unwrap_or(0)
        .max(60);
    super::dialog(f, area, cells(width), " Delete ", lines)
}

/// What the dialog asks, for the method chosen.
fn question<'a>(c: &Confirm, t: &Theme) -> Vec<Line<'a>> {
    let sent: Vec<&Item> = c.sent().collect();
    let what = match sent.as_slice() {
        [one] => one.label(),
        _ => format!("{} entries", sent.len()),
    };
    match c.mode {
        Mode::Trash => vec![Line::raw(format!(" Move {what} to the Trash?"))],
        Mode::Stage => vec![
            Line::raw(format!(" Stage {what} for deletion?")),
            Line::raw(format!(
                " Deleted after {} s; U puts it back until then.",
                dfdu_engine::STAGE_UNDO_WINDOW.as_secs()
            )),
        ],
        Mode::Permanent => vec![
            Line::styled(format!(" Permanently delete {what}?"), t.status_error),
            Line::raw(" This cannot be undone."),
        ],
    }
}

/// The size of the one target, or the targets listed with their total.
fn targets(lines: &mut Vec<Line<'_>>, c: &Confirm, t: &Theme) {
    if let [one] = c.targets.as_slice() {
        lines.push(Line::styled(
            if one.scanning {
                format!(" at least {} (still scanning)", human(one.size))
            } else {
                format!(" {}", human(one.size))
            },
            t.fg_dim,
        ));
    } else {
        for it in c.targets.iter().take(SHOWN) {
            let mut spans = vec![Span::raw(format!(
                " {:>10}  {}",
                human(it.size),
                it.label()
            ))];
            if it.inside {
                spans.push(Span::styled("  inside another marked item", t.fg_dim));
            } else if it.scanning {
                spans.push(Span::styled("  still scanning: at least this", t.fg_dim));
            }
            lines.push(Line::from(spans));
        }
        if c.targets.len() > SHOWN {
            lines.push(Line::styled(
                format!(" {:>10}  and {} more", "", c.targets.len() - SHOWN),
                t.fg_dim,
            ));
        }
        let mut total = vec![Span::raw(format!(
            " {:>10}  total · {} items",
            human(c.total),
            count(c.items)
        ))];
        if c.scanning() {
            total.push(Span::styled("  still scanning: at least this", t.fg_dim));
        }
        lines.push(Line::from(total));
    }
}

/// What the engine estimates the delete frees, or else what may keep it from freeing it all.
fn frees(lines: &mut Vec<Line<'_>>, app: &App, c: &Confirm) {
    let t = &app.theme;
    match app.snap.estimate.as_ref().filter(|e| e.id == c.estimate) {
        Some(e) => {
            let mut s = format!(
                " Frees {}{}",
                if e.scanning { "at least " } else { "about " },
                human(e.bytes)
            );
            let stays: Vec<String> = [
                (e.linked_out, "hard-linked from outside"),
                (e.shared_out, "shared with data outside the scan"),
            ]
            .iter()
            .filter(|(b, _)| *b > 0)
            .map(|(b, why)| format!("{} {why}", human(*b)))
            .collect();
            if !stays.is_empty() {
                s.push_str("; stays: ");
                s.push_str(&stays.join(", "));
            }
            if e.may_free_less {
                s.push_str("; may free less (clones)");
            }
            lines.push(Line::styled(
                s,
                if e.linked_out + e.shared_out > 0 {
                    t.status_warn
                } else {
                    t.fg_dim
                },
            ));
        }
        None if c.shared_out > 0 => lines.push(Line::styled(
            format!(
                " May free less: {} of it is also used outside the scan.",
                human(c.shared_out)
            ),
            t.status_warn,
        )),
        None => {}
    }
}

/// The method with the keys that change it, why the word must be typed, and the keys that
/// confirm.
fn method<'a>(lines: &mut Vec<Line<'a>>, c: &'a Confirm, t: &Theme) {
    let (method, other) = match c.mode {
        Mode::Trash => ("Trash, recoverable", "delete permanently"),
        Mode::Stage => ("stage, then delete", "delete permanently"),
        Mode::Permanent => ("delete permanently", "move to the Trash"),
    };
    let staging = if c.mode == Mode::Stage {
        "the Trash"
    } else {
        "stage"
    };
    lines.push(Line::from(vec![
        Span::raw(format!(" Method: {method}   ")),
        Span::styled("p", t.key),
        Span::raw(format!(" {other}  ")),
        Span::styled("s", t.key),
        Span::raw(format!(" {staging}")),
    ]));
    if c.mode != Mode::Permanent && c.typing {
        let why = if c.total >= LARGE {
            format!(" {} or more: confirm by typing the word.", human(LARGE))
        } else {
            format!(
                " Still scanning, may be {} or more: confirm by typing the word.",
                human(LARGE)
            )
        };
        lines.push(Line::styled(why, t.fg_dim));
    }
    lines.push(if c.typing {
        let mut spans = vec![
            Span::raw(" type "),
            Span::styled(WORD, t.key),
            Span::raw(" and Enter: "),
        ];
        spans.extend(input_spans(&c.typed, Style::new(), t.key));
        spans.extend([
            Span::raw("   "),
            Span::styled("Esc", t.key),
            Span::raw(" cancel"),
        ]);
        Line::from(spans)
    } else {
        Line::from(vec![
            Span::styled(" y", t.key),
            Span::raw(" confirm   "),
            Span::styled("n Esc", t.key),
            Span::raw(" cancel"),
        ])
    });
}

#[cfg(test)]
mod tests;
