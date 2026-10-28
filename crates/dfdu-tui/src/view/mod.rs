//! Drawing. Works only from [`App`] and its snapshot, never from the engine model, so it runs
//! after the read lock is released. [`draw`] lays out the screen, draws the header, the list
//! or the treemap, and the footer (modules here, the treemap in its own), the details pane, the debug panel and the open overlay (each in its
//! feature's module), and last lets motion effects work on the finished buffer.

mod footer;
mod header;
mod list;

use std::time::Instant;

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use crate::app::{App, Overlay};
use crate::{confirm, debug, details, error_list, filter, help, motion, palette, treemap};

/// Where the last frame drew each part of the screen, for mapping mouse positions.
#[derive(Copy, Clone, Debug, Default)]
pub struct Areas {
    pub header: Rect,
    pub list: Rect,
    pub details: Option<Rect>,
    pub status: Rect,
    pub footer: Rect,
    /// The open overlay.
    pub overlay: Option<Rect>,
    /// The debug panel, over the list.
    pub debug: Option<Rect>,
}

fn mmss(d: std::time::Duration) -> String {
    let s = d.as_secs();
    format!("{:02}:{:02}", s / 60, s % 60)
}

/// Draws the whole screen and updates the list height, scroll offset and areas in `app`.
pub fn draw(f: &mut Frame, app: &mut App, now: Instant) {
    let area = f.area();
    let stalled_line = if app.snap.stalled > 0 { 1 } else { 0 };
    let filter_line = filter::line(app);
    let [header, body, prompt, filter_row, status, footer] = Layout::vertical([
        Constraint::Length(2),
        Constraint::Min(1),
        Constraint::Length(stalled_line),
        Constraint::Length(filter_line.is_some() as u16),
        Constraint::Length(1),
        Constraint::Length(1),
    ])
    .areas(area);
    let (list, pane) = details::split(app, body);
    app.areas = Areas {
        header,
        list,
        details: pane,
        status,
        footer,
        overlay: None,
        debug: None,
    };
    let frame = now.duration_since(app.started).as_millis() as u64 / 100;

    header::draw(f, app, header, now, frame);
    app.treemap.shown = app.treemap.on && treemap::fits(area);
    if crate::dupes::active(app) {
        crate::dupes::draw(f, app, list);
    } else if crate::cleanup::active(app) {
        crate::cleanup::draw(f, app, list);
    } else if app.treemap.shown {
        treemap::draw(f, app, list, now);
    } else {
        list::draw(f, app, list, now, frame);
    }
    if stalled_line > 0 {
        footer::stalled(f, app, prompt);
    }
    if let Some(line) = filter_line {
        f.render_widget(Paragraph::new(line), filter_row);
    }
    footer::status(f, app, status);
    footer::hints(f, app, footer);

    if let Some(pane) = pane {
        details::draw(f, app, pane);
    }
    app.areas.debug = debug::draw(f, app, body);
    app.areas.overlay = match &app.overlay {
        Overlay::None => None,
        Overlay::Help(_) => Some(help::draw(f, app, area)),
        Overlay::Errors { .. } => Some(error_list::draw(f, app, area)),
        Overlay::ConfirmDelete { .. } => Some(confirm::draw(f, app, area)),
        Overlay::Palette { .. } => Some(palette::draw(f, app, area)),
        Overlay::Types(_) => Some(crate::types::draw(f, app, area)),
    };
    motion::post_process(app, f.buffer_mut(), now);
}

/// A line with `left` at the start and `right` at the end of `width` cells.
fn spread<'a>(left: Vec<Span<'a>>, right: Vec<Span<'a>>, width: u16) -> Line<'a> {
    let lw: usize = left.iter().map(|s| s.width()).sum();
    let rw: usize = right.iter().map(|s| s.width()).sum();
    let pad = (width as usize).saturating_sub(lw + rw);
    let mut spans = left;
    spans.push(Span::raw(" ".repeat(pad)));
    spans.extend(right);
    Line::from(spans)
}
