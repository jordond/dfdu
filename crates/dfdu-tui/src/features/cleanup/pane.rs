//! The native command pane (`n`): an item's own cleanup command, its dry run first when it has
//! one, then `y` runs it; `v` picks a further-reaching form, which `y` runs only when pressed
//! twice. A read-only browser runs only the dry runs.

use dfdu_engine::Command;
use dfdu_tree::{NodeRef, RuleIx};
use ratatui::Frame;
use ratatui::crossterm::event::{KeyCode, KeyEvent};
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Clear, Paragraph};

use super::{Item, selected_item};
use crate::app::{App, Effect};

/// The native command pane of one item.
#[derive(Clone, Debug)]
pub(super) struct Pane {
    pub(super) node: NodeRef,
    pub(super) rule: RuleIx,
    pub(super) variant: Option<usize>,
    /// The command whose output is shown; 0 before anything ran (no dry run to show).
    pub(super) id: u64,
    pub(super) dry_run: bool,
    /// A variant's real run was asked for once; asking again runs it.
    pub(super) armed: bool,
}

/// `N` ([`Action::CleanupCommand`](crate::app::keys::Action)): opens the selected item's
/// native command, starting with its dry run if it has one.
pub fn command(app: &mut App) -> Vec<Effect> {
    let Some(it) = selected_item(app).cloned() else {
        return Vec::new();
    };
    let Some(rules) = app.snap.rules.clone() else {
        return Vec::new();
    };
    let Some(def) = rules.rule(it.class.rule).action.native() else {
        app.status = Some(if app.read_only {
            "no native command for this item".into()
        } else {
            "no native command for this item; d or D deletes it".into()
        });
        return Vec::new();
    };
    if def.dry_run.is_empty() {
        app.cleanup.pane = Some(Pane {
            node: it.node,
            rule: it.class.rule,
            variant: None,
            id: 0,
            dry_run: true,
            armed: false,
        });
        return Vec::new();
    }
    start(app, &it, None, true)
}

fn start(app: &mut App, it: &Item, variant: Option<usize>, dry_run: bool) -> Vec<Effect> {
    app.cleanup.commands += 1;
    // Ids from the view are above any the dialog uses.
    let id = (1 << 32) + app.cleanup.commands;
    app.cleanup.pane = Some(Pane {
        node: it.node,
        rule: it.class.rule,
        variant,
        id,
        dry_run,
        armed: false,
    });
    vec![Effect::Send(Command::RunNative {
        id,
        node: it.node,
        variant,
        dry_run,
    })]
}

pub(super) fn key(app: &mut App, k: &KeyEvent) -> Vec<Effect> {
    let Some(p) = app.cleanup.pane.clone() else {
        return Vec::new();
    };
    let Some(rules) = app.snap.rules.clone() else {
        return Vec::new();
    };
    let Some(def) = rules.rule(p.rule).action.native() else {
        return Vec::new();
    };
    // Until its result has arrived, the shown command counts as running.
    let done = app
        .extras
        .cleanup
        .native
        .as_ref()
        .filter(|n| n.id == p.id)
        .and_then(|n| n.done.clone());
    let running = p.id != 0 && done.is_none();
    // The real run only after nothing ran yet (no dry run to show) or a dry run that worked.
    let may_run = p.id == 0 || (p.dry_run && done == Some(Ok(())));
    if app.read_only && may_run && k.code == KeyCode::Char('y') {
        app.status = Some(crate::app::keys::READ_ONLY.into());
        return Vec::new();
    }
    let it = app
        .extras
        .cleanup
        .items
        .iter()
        .find(|i| i.node == p.node)
        .cloned();
    match k.code {
        KeyCode::Esc | KeyCode::Char('q') if !running => app.cleanup.pane = None,
        KeyCode::Char('v') if !running => {
            let next = match p.variant {
                None if !def.variants.is_empty() => Some(0),
                Some(v) if v + 1 < def.variants.len() => Some(v + 1),
                _ => None,
            };
            let dry = match next {
                None => !def.dry_run.is_empty(),
                Some(v) => !def.variants[v].dry_run.is_empty(),
            };
            if let Some(it) = it {
                if dry {
                    return start(app, &it, next, true);
                }
                app.cleanup.pane = Some(Pane {
                    variant: next,
                    id: 0,
                    dry_run: true,
                    armed: false,
                    ..p
                });
            }
        }
        KeyCode::Char('y') if may_run => {
            let Some(it) = it else {
                return Vec::new();
            };
            if p.variant.is_some() && !p.armed {
                if let Some(pane) = app.cleanup.pane.as_mut() {
                    pane.armed = true;
                }
                app.status = Some("press y again to run it".into());
                return Vec::new();
            }
            return start(app, &it, p.variant, false);
        }
        _ => {}
    }
    Vec::new()
}

/// Draws the pane over the view in `area`.
pub(super) fn draw(f: &mut Frame, app: &App, area: Rect, t: &crate::theme::Theme) {
    let Some(p) = &app.cleanup.pane else {
        return;
    };
    let Some(rules) = &app.snap.rules else {
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
    let shown = app.extras.cleanup.native.as_ref().filter(|n| n.id == p.id);
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
    let ro = app.read_only;
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
