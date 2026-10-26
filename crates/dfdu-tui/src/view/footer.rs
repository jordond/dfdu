//! The lines below the list: the prompt about directories that do not respond, the status line
//! and the key hints.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use super::spread;
use crate::app::App;
use crate::marks;

/// Drawn only while directories are stalled (the row has no height otherwise).
pub fn stalled(f: &mut Frame, app: &App, area: Rect) {
    let n = app.snap.stalled;
    let text = format!(
        " {n} director{} not responding: wait, or press x to skip {}",
        if n == 1 { "y is" } else { "ies are" },
        if n == 1 { "it" } else { "them" }
    );
    f.render_widget(Paragraph::new(Line::styled(text, app.theme.warn)), area);
}

/// Delete progress, else the last status message or engine notice; what is marked on the right.
pub fn status(f: &mut Frame, app: &App, area: Rect) {
    let msg = match app.snap.deleting {
        Some((done, total)) => format!("deleting {done}/{total}…"),
        None => app
            .status
            .clone()
            .or_else(|| app.snap.notice.clone())
            .unwrap_or_default(),
    };
    let line = match marks::summary(app) {
        Some(marked) => spread(
            vec![Span::raw(format!(" {msg}"))],
            vec![marked, Span::raw(" ")],
            area.width,
        )
        .style(app.theme.dim),
        None => Line::styled(format!(" {msg}"), app.theme.dim),
    };
    f.render_widget(Paragraph::new(line), area);
}

pub fn hints(f: &mut Frame, app: &App, area: Rect) {
    let hints = if app.snap.root_total.errors > 0 {
        "↑↓ move  → open  ← up  s n C M sort  a u size  r rescan  d D delete  ! errors  ? help  q quit"
    } else {
        "↑↓ move  → open  ← up  s n C M sort  a u size  r rescan  d trash  D delete  ? help  q quit"
    };
    f.render_widget(
        Paragraph::new(Line::styled(format!(" {hints}"), app.theme.dim)),
        area,
    );
}
