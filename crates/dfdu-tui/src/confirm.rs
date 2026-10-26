//! The delete confirmation: `y` moves the selected entry to the Trash, a permanent delete needs
//! the word "delete" typed and `Enter`.

use dfdu_engine::{Command, Mode};
use ratatui::Frame;
use ratatui::crossterm::event::{KeyCode, KeyEvent};
use ratatui::layout::{Constraint, Rect};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Clear, Paragraph};

use crate::app::{App, Effect, Overlay};
use crate::snapshot::Mark;
use crate::theme::human;

/// Asks to delete the selected entry. The "shared between entries" row cannot be deleted.
pub fn open(app: &mut App, mode: Mode) {
    if let Some(e) = app.selected_entry().filter(|e| !e.pseudo) {
        app.overlay = Overlay::ConfirmDelete {
            node: e.node,
            name: String::from_utf8_lossy(&e.name).into_owned(),
            size: e.size,
            shared_out: e.shared_out,
            is_dir: e.is_dir,
            mode,
            typed: String::new(),
            scanning: matches!(e.mark, Mark::Scanning | Mark::Stalled | Mark::Rescanning),
        };
    }
}

pub fn key(app: &mut App, k: KeyEvent) -> Vec<Effect> {
    let Overlay::ConfirmDelete {
        node, mode, typed, ..
    } = &app.overlay
    else {
        return Vec::new();
    };
    let (node, mode) = (*node, *mode);
    let mut typed = typed.clone();
    let confirmed = match (mode, k.code) {
        (_, KeyCode::Esc) => false,
        (Mode::Trash, KeyCode::Char('y' | 'Y')) => true,
        (Mode::Trash, _) => false,
        (Mode::Permanent, KeyCode::Enter) => {
            if typed != "delete" {
                app.status = Some("not deleted: type delete to confirm".into());
            }
            typed == "delete"
        }
        (Mode::Permanent, KeyCode::Backspace) => {
            typed.pop();
            set_typed(app, typed);
            return Vec::new();
        }
        (Mode::Permanent, KeyCode::Char(c)) if typed.len() < 16 => {
            typed.push(c);
            set_typed(app, typed);
            return Vec::new();
        }
        (Mode::Permanent, _) => return Vec::new(),
    };
    let mut fx = Vec::new();
    if confirmed {
        fx.push(Effect::Send(Command::Delete {
            items: vec![node],
            mode,
        }));
        app.status = Some(match mode {
            Mode::Trash => "moving to the Trash…".into(),
            Mode::Permanent => "deleting…".into(),
        });
    }
    app.overlay = Overlay::None;
    fx
}

fn set_typed(app: &mut App, t: String) {
    if let Overlay::ConfirmDelete { typed, .. } = &mut app.overlay {
        *typed = t;
    }
}

/// Draws the dialog centred in `area` and returns the rectangle it covers.
pub fn draw(f: &mut Frame, app: &App, area: Rect) -> Rect {
    let Overlay::ConfirmDelete {
        name,
        size,
        shared_out,
        is_dir,
        mode,
        typed,
        scanning,
        ..
    } = &app.overlay
    else {
        return Rect::default();
    };
    let what = if *is_dir {
        format!("{name}/")
    } else {
        name.clone()
    };
    let size_line = if *scanning {
        format!(" at least {} (still scanning)", human(*size))
    } else {
        format!(" {}", human(*size))
    };
    let mut lines = match mode {
        Mode::Trash => vec![Line::raw(format!(" Move {what} to the Trash?"))],
        Mode::Permanent => vec![
            Line::styled(format!(" Permanently delete {what}?"), app.theme.error),
            Line::raw(" This cannot be undone."),
        ],
    };
    lines.push(Line::styled(size_line, app.theme.dim));
    if *shared_out > 0 {
        lines.push(Line::styled(
            format!(
                " May free less: {} of it is also used outside the scan.",
                human(*shared_out)
            ),
            app.theme.warn,
        ));
    }
    lines.push(Line::raw(""));
    lines.push(match mode {
        Mode::Trash => Line::from(vec![
            Span::styled(" y", app.theme.key),
            Span::raw(" confirm   "),
            Span::styled("n Esc", app.theme.key),
            Span::raw(" cancel"),
        ]),
        Mode::Permanent => Line::from(vec![
            Span::raw(" type "),
            Span::styled("delete", app.theme.key),
            Span::raw(" and Enter: "),
            Span::raw(typed.as_str()),
            Span::raw("   "),
            Span::styled("Esc", app.theme.key),
            Span::raw(" cancel"),
        ]),
    });
    // Wide enough for the longest line (a long name, the "may free less" note), at least 60.
    let width = lines
        .iter()
        .map(|l| l.width() + 3)
        .max()
        .unwrap_or(0)
        .max(60);
    let r = area.centered(
        Constraint::Length(width.min(u16::MAX as usize) as u16),
        Constraint::Length(lines.len() as u16 + 2),
    );
    f.render_widget(Clear, r);
    f.render_widget(
        Paragraph::new(lines).block(Block::bordered().title(" Delete ")),
        r,
    );
    r
}

#[cfg(test)]
mod tests {
    use std::time::Instant;

    use super::*;
    use crate::app::tests::snap;
    use crate::snapshot::test_entry;
    use crate::tests::key;
    use crate::theme::Theme;

    #[test]
    fn trash_delete_sends_a_plan_after_y() {
        let t0 = Instant::now();
        let mut app = App::new(Theme::no_color(), t0);
        app.refresh(snap(vec![test_entry(0, "a", 10)], 1), t0);
        assert!(app.key(key(KeyCode::Char('d')), t0).is_empty());
        assert!(matches!(
            app.overlay,
            Overlay::ConfirmDelete {
                mode: Mode::Trash,
                ..
            }
        ));
        let fx = app.key(key(KeyCode::Char('y')), t0);
        assert!(matches!(
            fx.as_slice(),
            [Effect::Send(Command::Delete {
                mode: Mode::Trash,
                ..
            })]
        ));
        assert!(matches!(app.overlay, Overlay::None));
    }

    #[test]
    fn permanent_delete_needs_the_word_delete() {
        let t0 = Instant::now();
        let mut app = App::new(Theme::no_color(), t0);
        app.refresh(snap(vec![test_entry(0, "a", 10)], 1), t0);
        app.key(key(KeyCode::Char('D')), t0);
        // 'y' alone does nothing for a permanent delete.
        assert!(app.key(key(KeyCode::Char('y')), t0).is_empty());
        app.key(key(KeyCode::Backspace), t0);
        for c in "delet".chars() {
            assert!(app.key(key(KeyCode::Char(c)), t0).is_empty());
        }
        assert!(
            app.key(key(KeyCode::Enter), t0).is_empty(),
            "incomplete word must not delete"
        );
        assert!(matches!(app.overlay, Overlay::None));
        app.key(key(KeyCode::Char('D')), t0);
        for c in "delete".chars() {
            app.key(key(KeyCode::Char(c)), t0);
        }
        let fx = app.key(key(KeyCode::Enter), t0);
        assert!(matches!(
            fx.as_slice(),
            [Effect::Send(Command::Delete {
                mode: Mode::Permanent,
                ..
            })]
        ));
    }
}
