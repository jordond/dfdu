//! The two header lines. The first is the tab bar: the five views by number and name, the one
//! shown highlighted, with what Cleanup and Duplicates hold, and the scan state on the right.
//! The second is the scope line: what the view covers (the breadcrumbs from the scan root to
//! the current directory, or the whole scan for Cleanup and Duplicates), then the totals and the
//! modes that change the numbers: the sort, the size shown, and what differs from the defaults.
//! A view's own state (the map's colouring and depth, the duplicates' keep rule) is drawn in the
//! view, so the header means the same in every view.

use std::time::Instant;

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use super::{cells, mmss, spread};
use crate::app::App;
use crate::app::views::{self, View};
use crate::format::{count, human, spinner, truncate_middle};
use crate::input::mouse::{Crumb, Tab};
use crate::model::cache_link::{Cached, ago};
use crate::model::outside::tilde;

mod crumbs;
mod modes;
mod tabs;

use crumbs::{breadcrumbs, least_width, segments};
use modes::{WIDE_FROM, mode_forms};
use tabs::{TABS_NAMED_FROM, Tabs, tabs};

/// The right of the first line: whether the browser is read-only, and the scan's state. Longest
/// first, then the shorter forms for narrow terminals: `read-only · done in 00:16`,
/// `read-only · done 00:16`, `done 00:16`, `00:16`. While scanning, each keeps the spinner. A
/// cached scan says so until it is up to date: `cached 3 h ago, refreshing 00:02`.
fn scan_states(app: &App, now: Instant, frame: u64) -> Vec<Vec<Span<'static>>> {
    let ((long, short, bare), style) = match (app.cached, app.done_at) {
        (Some(cached), _) => {
            let (spin, time) = (spinner(frame), mmss(now.duration_since(app.started)));
            let (long, short) = match cached {
                Cached::Loading => (
                    format!("{spin} loading the cached scan {time}"),
                    format!("{spin} loading cache"),
                ),
                Cached::Refreshing { written } => {
                    let age = ago(written, dfdu_tree::now_secs().max(0).cast_unsigned());
                    (
                        format!("{spin} cached {age} ago, refreshing {time}"),
                        format!("{spin} cached {age} ago"),
                    )
                }
            };
            (
                (long, short, format!("{spin} {time}")),
                app.theme.scan_active,
            )
        }
        (None, Some(done)) if !app.snap.busy => {
            let time = mmss(done.duration_since(app.started));
            (
                (format!("done in {time}"), format!("done {time}"), time),
                app.theme.scan_done,
            )
        }
        _ => {
            let (spin, time) = (spinner(frame), mmss(now.duration_since(app.started)));
            let scanning = format!("{spin} scanning {time}");
            (
                (scanning.clone(), scanning, format!("{spin} {time}")),
                app.theme.scan_active,
            )
        }
    };
    let state = |text: &str| vec![Span::styled(text.to_owned(), style), Span::raw(" ")];
    let read_only = |text: &str| {
        let mut spans = vec![
            Span::styled("read-only", app.theme.status_warn),
            Span::styled(" · ", app.theme.fg_dim),
        ];
        spans.extend(state(text));
        spans
    };
    let mut forms = Vec::new();
    if app.read_only {
        forms.extend([read_only(&long), read_only(&short)]);
    } else {
        forms.push(state(&long));
    }
    forms.extend([state(&short), state(&bare)]);
    forms
}

/// Between the totals and the modes on the second line.
const SCOPE_GAP: &str = "   ";
/// Before the scan root on the scope line of the views that cover the whole scan.
const WHOLE_SCAN: &str = "whole scan: ";
/// The most cells of the current directory's name the scope line keeps before the modes give
/// way: a longer name is shortened in the middle instead.
const SCOPE_KEPT: usize = 24;
/// The fewest cells of the scan root the whole-scan scope keeps before the modes give way.
const WHOLE_SCAN_ROOT_MIN: usize = 8;

fn width(spans: &[Span]) -> usize {
    spans.iter().map(Span::width).sum()
}

/// The totals after the scope on the second line: items and size of the scan, then errors,
/// the type filter and what is still being measured.
fn totals(app: &App) -> Vec<Span<'static>> {
    let t = &app.theme;
    let s = &app.snap;
    let root = s.root_total;
    let size = app.size_mode.of(&root);
    let mut info = vec![
        Span::styled(format!("{} items · ", count(root.items)), t.fg_dim),
        Span::styled(human(size), t.header),
    ];
    if root.errors > 0 {
        info.push(Span::styled(" · ", t.fg_dim));
        info.push(Span::styled(
            match root.errors {
                1 => "1 error".to_string(),
                n => format!("{} errors", count(n)),
            },
            t.status_error,
        ));
    }
    if !s.types.is_empty() {
        let d = s.dir_total;
        info.push(Span::styled(
            format!(
                " · only {}: {} in {} files here",
                s.types,
                human(app.size_mode.of(&d)),
                count(d.items)
            ),
            t.status_warn,
        ));
    }
    if s.incomplete {
        info.push(Span::styled(" · incomplete", t.status_warn));
    }
    if let Some((done, requested)) = s.measuring {
        // While a scan runs, measurement can catch up with what it has asked for so far: no
        // percentage then, as the scan asks for more.
        let text = if done < requested {
            format!(" · measuring shared data {}%", done * 100 / requested)
        } else {
            " · measuring shared data".to_string()
        };
        info.push(Span::styled(text, t.fg_dim));
    }
    if s.unmeasured > 0 {
        info.push(Span::styled(
            format!(" · {} files not measured", count(s.unmeasured)),
            t.status_warn,
        ));
    }
    if let Some((profile, ratio)) = s.profile
        && (ratio - 1.0).abs() > f64::EPSILON
    {
        info.push(Span::styled(
            format!(" · data stored {ratio:.1}× ({})", profile.name()),
            t.fg_dim,
        ));
    }
    if s.compressed_upper_bound {
        info.push(Span::styled(
            " · compressed sizes are upper bounds (exact as root)",
            t.fg_dim,
        ));
    }
    info
}

/// Draws the header in `area` and records where each tab and breadcrumb went, for the mouse.
pub fn draw(f: &mut Frame, app: &mut App, area: Rect, now: Instant, frame: u64) {
    let (line1, tabs) = tab_line(app, area, now, frame);
    let (line2, crumbs) = scope_line(app, area);
    f.render_widget(Paragraph::new(vec![line1, line2]), area);
    app.mouse.tabs = tabs;
    app.mouse.crumbs = crumbs;
}

/// The first line, the title, the tab bar and the scan state, and where each tab is on it.
///
/// The tab bar takes its fullest form that fits beside a form of the scan state, which shortens
/// first; one cell stays free between them. The bare time, which no longer says whether the scan
/// is done, comes only after every form of the tab bar.
fn tab_line(app: &App, area: Rect, now: Instant, frame: u64) -> (Line<'static>, Vec<Tab>) {
    let w = usize::from(area.width);
    let mut left = vec![Span::styled(" dfdu ", app.theme.title)];
    let start = width(&left);
    let mut bars = Vec::new();
    if area.width >= TABS_NAMED_FROM {
        bars.extend([tabs(app, true, true), tabs(app, true, false)]);
    }
    bars.push(tabs(app, false, false));
    let states = scan_states(app, now, frame);
    let fits = |bar: &Tabs, state: &[Span]| start + width(&bar.spans) + 1 + width(state) <= w;
    let (worded, bare) = states.split_at(states.len() - 1);
    let (bar, state) = [worded, bare]
        .into_iter()
        .find_map(|tier| {
            (bars.iter()).find_map(|b| tier.iter().find(|st| fits(b, st)).map(|st| (b, st)))
        })
        .unwrap_or((&bars[bars.len() - 1], &bare[0]));
    left.extend(bar.spans.iter().cloned());
    let hits = (bar.hits.iter())
        .filter(|h| area.height > 0 && start + h.x + h.w <= w)
        .map(|h| Tab {
            area: Rect::new(area.x + cells(start + h.x), area.y, cells(h.w), 1),
            view: h.view,
        })
        .collect();
    (spread(left, state.clone(), area.width), hits)
}

/// The second line, the scope and then the totals and modes, and where each breadcrumb is on it.
///
/// The scope keeps the current directory's name whole, up to [`SCOPE_KEPT`] cells of it (a long
/// name or scan root is shortened in the middle rather than hide the global state), or a few
/// cells of the scan root: the modes give way first, in their longest form that leaves it room.
/// Then the scope shortens, as the sort is never left out while the line has room for the totals
/// and it.
fn scope_line(app: &App, area: Rect) -> (Line<'static>, Vec<Crumb>) {
    let s = &app.snap;
    let w = usize::from(area.width);
    let lead = 1;
    let view = views::current(app);
    let whole_scan = view.whole_scan();
    // Before the first snapshot the root is not known: the directory opened is.
    let root_path = if s.root_path.is_empty() {
        &s.dir_path
    } else {
        &s.root_path
    };
    let root = tilde(root_path, app.home.as_deref());
    let segs = segments(&s.dir_path, &s.root_path, app.home.as_deref());
    let least = if whole_scan {
        Span::raw(WHOLE_SCAN).width() + Span::raw(&root).width().min(WHOLE_SCAN_ROOT_MIN)
    } else {
        least_width(&segs).min(SCOPE_KEPT)
    };
    let info = totals(app);
    let room = |right: &[Span]| w.saturating_sub(lead + width(right) + 1);
    let rights: Vec<Vec<Span>> = mode_forms(app)
        .into_iter()
        .skip(usize::from(area.width < WIDE_FROM))
        .map(|modes| {
            let mut right = info.clone();
            right.push(Span::raw(SCOPE_GAP));
            right.extend(modes);
            right
        })
        .collect();
    let right = (rights.iter())
        .find(|right| room(right) >= least)
        .or_else(|| rights.last().filter(|r| lead + width(r) < w))
        .cloned()
        .unwrap_or_else(|| {
            let mut right = info.clone();
            right.push(Span::raw(" "));
            right
        });
    let scope_room = room(&right);
    let mut scope = vec![Span::raw(" ")];
    let mut hits = Vec::new();
    if whole_scan {
        scope.extend(whole_scan_scope(app, view, &root, scope_room));
    } else {
        let (crumbs, crumb_hits) = breadcrumbs(&segs, scope_room, &app.theme);
        scope.extend(crumbs);
        hits = crumb_hits;
    }
    let crumbs = hits
        .into_iter()
        .filter(|&(x, cw, _)| area.height > 1 && lead + x + cw <= w)
        .map(|(x, cw, i)| Crumb {
            area: Rect::new(area.x + cells(lead + x), area.y + 1, cells(cw), 1),
            child: segs[i + 1].dir.clone(),
        })
        .collect();
    (spread(scope, right, area.width), crumbs)
}

/// The scope of a view that covers the whole scan, in `room` cells: [`WHOLE_SCAN`], the scan
/// root, and what the view covers of it when that fits whole. Without room for a few cells of
/// the root, only that it is the whole scan.
fn whole_scan_scope(app: &App, view: View, root: &str, room: usize) -> Vec<Span<'static>> {
    let t = &app.theme;
    let rest = room.saturating_sub(Span::raw(WHOLE_SCAN).width());
    if root.is_empty() || rest < Span::raw(root).width().min(WHOLE_SCAN_ROOT_MIN) {
        let short = truncate_middle(WHOLE_SCAN.trim_end_matches([':', ' ']), room);
        return vec![Span::styled(short, t.fg_dim)];
    }
    let mut scope = vec![
        Span::styled(WHOLE_SCAN, t.fg_dim),
        Span::styled(truncate_middle(root, rest), t.title),
    ];
    let note = views::scope_note(view).map(|n| format!(" · {n}"));
    let room_after_root = rest.saturating_sub(Span::raw(root).width());
    if let Some(note) = note.filter(|n| Span::raw(n).width() <= room_after_root) {
        scope.push(Span::styled(note, t.fg_dim));
    }
    scope
}

#[cfg(test)]
mod tests;
