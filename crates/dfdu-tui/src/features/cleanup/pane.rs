//! The native command pane (`n`) drawn: an item's own cleanup command, its dry run first when
//! it has one, then `y` runs it; `v` picks a further-reaching form, which `y` runs only when
//! pressed twice. A read-only browser runs only the dry runs. The pane is the session's
//! (`dfdu_session::cleanup::pane`): the keys turn into its intents.

use dfdu_session::cleanup::pane::{self as session, Intent};
use ratatui::Frame;
use ratatui::crossterm::event::{KeyCode, KeyEvent};
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Clear, Paragraph};

use crate::app::{App, Effect};

/// `N` ([`Action::CleanupCommand`](crate::app::keys::Action)): opens the selected item's
/// native command ([`session::open`]).
pub fn command(app: &mut App) -> Vec<Effect> {
    session::open(&mut app.session)
}

/// A key while the pane is open: `Esc` and `q` close it, `v` picks another form and `y` runs
/// it; the other keys do nothing.
pub(super) fn key(app: &mut App, k: &KeyEvent) -> Vec<Effect> {
    let intent = match k.code {
        KeyCode::Esc | KeyCode::Char('q') => Intent::Close,
        KeyCode::Char('v') => Intent::OtherForm,
        KeyCode::Char('y') => Intent::Run,
        _ => return Vec::new(),
    };
    session::act(&mut app.session, intent)
}

/// Draws the pane over the view in `area`.
pub(super) fn draw(f: &mut Frame, app: &App, area: Rect, t: &crate::theme::Theme) {
    let Some(p) = &app.session.cleanup.pane else {
        return;
    };
    let Some(rules) = &app.session.snap.rules else {
        return;
    };
    let rule = rules.rule(p.rule);
    let Some(def) = rule.action.native() else {
        return;
    };
    let width = area.width.saturating_sub(4).max(20);
    let height = area.height.saturating_sub(2).max(6);
    let rect = Rect::new(
        area.x + 2,
        area.y + 1,
        width.min(area.width),
        height.min(area.height),
    );
    f.render_widget(Clear, rect);
    let shown = session::output(&app.session, p);
    let what = match p.variant {
        None => "its cleanup command".to_string(),
        Some(v) => def
            .variants
            .get(v)
            .map_or(String::new(), |x| x.name.clone()),
    };
    let mut lines = vec![Line::from(vec![
        Span::styled(format!(" {}: ", rule.name), t.title),
        Span::raw(what),
    ])];
    let body = rect.height.saturating_sub(5) as usize;
    if let Some(output) = shown {
        lines.extend(
            output
                .lines
                .iter()
                .skip(output.lines.len().saturating_sub(body))
                .map(|l| Line::raw(format!(" {l}"))),
        );
    }
    lines.push(Line::raw(""));
    let command = match p.variant {
        None => &def.command,
        Some(v) => def.variants.get(v).map_or(&def.command, |x| &x.command),
    };
    let ro = app.session.read_only;
    // What `y` and the other keys do once the command can run.
    let keys = |intro: String| {
        let mut spans = vec![Span::raw(intro)];
        if !ro {
            spans.extend([Span::styled("y", t.key), Span::raw(" run it  ")]);
        }
        spans.extend([
            Span::styled("v", t.key),
            Span::raw(" other form  "),
            Span::styled("Esc", t.key),
            Span::raw(" close"),
        ]);
        Line::from(spans)
    };
    let state = match shown.and_then(|n| n.done.as_ref()) {
        None if p.id == 0 => {
            let argv = dfdu_engine::shown_argv(command);
            keys(if ro {
                format!(" Would run {argv} (not in read-only mode). ")
            } else {
                format!(" Runs {argv}. ")
            })
        }
        None => Line::styled(" running…", t.fg_dim),
        Some(Ok(())) if p.dry_run => keys(if ro {
            " That was the dry run; read-only mode runs nothing more. ".into()
        } else {
            " That was the dry run. ".into()
        }),
        Some(Ok(())) => Line::styled(" Done; the directory is rescanned.", t.status_ok),
        Some(Err(e)) => Line::styled(format!(" Failed: {e}"), t.status_error),
    };
    lines.push(state);
    if p.armed {
        lines.push(Line::styled(
            " This form removes more than the default: press y again to run it.",
            t.status_warn,
        ));
    }
    f.render_widget(
        Paragraph::new(lines)
            .block(Block::bordered().border_style(t.border_focus))
            .style(Style::new()),
        rect,
    );
}
