//! The delete confirmation drawn, for the marked entries or else the selected one. It lists the
//! targets, their total and the method: `y` moves them to the Trash, `p` switches between the
//! Trash and a permanent delete, `s` between the Trash and staging (renamed aside at once, put
//! back with `U` for a minute, then deleted). A permanent delete, and a move to the Trash of
//! 50 GiB or more or of a directory still being scanned, needs the word "delete" typed and
//! `Enter`. The dialog and its gate are the session's (`dfdu_session::confirm`): the keys turn
//! into its intents.

use dfdu_engine::Mode;
use dfdu_session::confirm::{self as session, Intent, LARGE, WORD};
use ratatui::Frame;
use ratatui::crossterm::event::{KeyCode, KeyEvent};
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::{Line, Span};

use crate::app::{App, Effect, views};
use crate::format::{count, human};
use crate::input::line_keys;
use crate::theme::Theme;
use crate::view::{cells, input_spans};
pub use session::{Confirm, Item};

/// Targets listed before "and N more".
const SHOWN: usize = 8;

/// Asks to delete the marked entries or the selected one ([`session::open`]).
pub fn open(app: &mut App, mode: Mode) -> Vec<Effect> {
    session::open(&mut app.session, mode)
}

/// Asks to delete the marked entries among `only` ([`session::open_only`]).
pub fn open_only(
    app: &mut App,
    mode: Mode,
    only: &std::collections::HashSet<dfdu_tree::NodeRef>,
) -> Vec<Effect> {
    session::open_only(&mut app.session, mode, only)
}

/// True while the dialog is open: it takes every key and is drawn over everything else.
pub fn is_open(app: &App) -> bool {
    app.session.confirm.is_some()
}

/// Handles a key while the dialog is open. `Esc` and `n` cancel, `p` and `s` switch the
/// method (`s` only before anything is typed), and without the word to type `y` confirms and
/// any other key cancels; with it the keys edit the word and `Enter` confirms.
pub fn key(app: &mut App, k: KeyEvent) -> Vec<Effect> {
    let Some(c) = &app.session.confirm else {
        return Vec::new();
    };
    let intent = match k.code {
        KeyCode::Esc | KeyCode::Char('n') => Intent::Cancel,
        KeyCode::Char('p' | 'P') => Intent::SwitchPermanent,
        KeyCode::Char('s' | 'S') if !c.typing() || c.typed().is_empty() => Intent::SwitchStage,
        KeyCode::Char('y' | 'Y') if !c.typing() => Intent::Confirm,
        _ if !c.typing() => Intent::Cancel,
        _ => match line_keys::op(&k) {
            Some(op) => Intent::Text(op),
            None => return Vec::new(),
        },
    };
    let then = session::act(&mut app.session, intent);
    views::carry(app, then)
}

/// Draws the dialog centred in `area` and returns the rectangle it covers.
pub fn draw(f: &mut Frame, app: &App, area: Rect) -> Rect {
    let Some(c) = &app.session.confirm else {
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
    match c.mode() {
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
    if let [one] = c.targets() {
        lines.push(Line::styled(
            if one.scanning {
                format!(" at least {} (still scanning)", human(one.size))
            } else {
                format!(" {}", human(one.size))
            },
            t.fg_dim,
        ));
    } else {
        for it in c.targets().iter().take(SHOWN) {
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
        if c.targets().len() > SHOWN {
            lines.push(Line::styled(
                format!(" {:>10}  and {} more", "", c.targets().len() - SHOWN),
                t.fg_dim,
            ));
        }
        let mut total = vec![Span::raw(format!(
            " {:>10}  total · {} items",
            human(c.total()),
            count(c.items())
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
    match app
        .session
        .snap
        .estimate
        .as_ref()
        .filter(|e| e.id == c.estimate())
    {
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
        None if c.shared_out() > 0 => lines.push(Line::styled(
            format!(
                " May free less: {} of it is also used outside the scan.",
                human(c.shared_out())
            ),
            t.status_warn,
        )),
        None => {}
    }
}

/// The method with the keys that change it, why the word must be typed, and the keys that
/// confirm.
fn method<'a>(lines: &mut Vec<Line<'a>>, c: &'a Confirm, t: &Theme) {
    let (method, other) = match c.mode() {
        Mode::Trash => ("Trash, recoverable", "delete permanently"),
        Mode::Stage => ("stage, then delete", "delete permanently"),
        Mode::Permanent => ("delete permanently", "move to the Trash"),
    };
    let staging = if c.mode() == Mode::Stage {
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
    if c.mode() != Mode::Permanent && c.typing() {
        let why = if c.total() >= LARGE {
            format!(" {} or more: confirm by typing the word.", human(LARGE))
        } else {
            format!(
                " Still scanning, may be {} or more: confirm by typing the word.",
                human(LARGE)
            )
        };
        lines.push(Line::styled(why, t.fg_dim));
    }
    lines.push(if c.typing() {
        let mut spans = vec![
            Span::raw(" type "),
            Span::styled(WORD, t.key),
            Span::raw(" and Enter: "),
        ];
        spans.extend(input_spans(c.typed(), Style::new(), t.key));
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
