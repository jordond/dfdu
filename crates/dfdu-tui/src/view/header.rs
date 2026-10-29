//! The two header lines. The first is the tab bar: the five views by number and name, the one
//! shown highlighted, with what Cleanup and Duplicates hold, and the scan state on the right.
//! The second is the scope line: what the view covers (the breadcrumbs from the scan root to
//! the current directory, or the whole scan for Cleanup and Duplicates), then the totals and the
//! modes that change the numbers: the sort, the size shown, and what differs from the defaults.
//! A view's own state (the map's colouring and depth, the duplicates' keep rule) is drawn in the
//! view, so the header means the same in every view.

use std::path::Path;
use std::time::Instant;

use dfdu_engine::SizeMode;
use dfdu_store::history;
use dfdu_tree::{Host, HostPath};
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::text::Span;
use ratatui::widgets::Paragraph;

use super::{cells, mmss, spread};
use crate::app::App;
use crate::app::views;
use crate::input::mouse::{Crumb, Tab};
use crate::model::growth;
use crate::model::outside::{rel_text, tilde};
use crate::state::filter;
use crate::state::order::SortKey;
use crate::theme::{Theme, count, human, spinner, truncate_middle};

mod tabs;

use tabs::{TABS_NAMED_FROM, Tabs, tabs};

/// Between two breadcrumbs.
const SEPARATOR: &str = " › ";
/// In place of the leading breadcrumbs that do not fit.
const COLLAPSED: &str = "…";

/// One breadcrumb: a directory on the way from the scan root to the current one.
#[derive(Debug, PartialEq)]
struct Segment {
    text: String,
    dir: HostPath,
}

/// The breadcrumbs of `dir`: the scan root `root` as its path, with the home directory `home`
/// shown as `~`, then one per directory below it. All of `dir` is one breadcrumb when it is not
/// below `root` (before the first snapshot). `home` is this machine's: another host's root is
/// shown as it is.
fn segments(dir: &HostPath, root: &HostPath, home: Option<&Path>) -> Vec<Segment> {
    let (root, below) = match dir.rel(root) {
        Some(below) if !root.is_empty() => (root, below),
        _ => (dir, Vec::new()),
    };
    // A home directory of `/` would turn every path into `~/...`.
    let home = home.filter(|h| h.parent().is_some() && root.host() == Host::LOCAL);
    let home = home.map(HostPath::local);
    let text = match home.as_ref().and_then(|h| rel_text(root, h)) {
        Some(rest) if rest.is_empty() => "~".to_owned(),
        Some(rest) => format!("~{}{rest}", char::from(root.host().sep())),
        None => root.to_string(),
    };
    let mut out = vec![Segment {
        text,
        dir: root.clone(),
    }];
    let mut dir = root.clone();
    for part in below {
        dir.push(part);
        out.push(Segment {
            text: String::from_utf8_lossy(part).into_owned(),
            dir: dir.clone(),
        });
    }
    out
}

/// The fewest cells [`breadcrumbs`] needs to show the last of `segs` whole: the last one, and
/// `… › ` before it when there are others.
fn least_width(segs: &[Segment]) -> usize {
    let last = Span::raw(&segs[segs.len() - 1].text).width();
    if segs.len() > 1 {
        Span::raw(COLLAPSED).width() + Span::raw(SEPARATOR).width() + last
    } else {
        last
    }
}

/// Lays out `segs` in at most `width` cells, putting `…` in place of leading breadcrumbs until
/// the rest fits; the last one is always shown, shortened in the middle when it does not fit by
/// itself (a long scan root). Returns the spans, and the column offset, width and index in
/// `segs` of each breadcrumb shown before the last.
fn breadcrumbs(
    segs: &[Segment],
    width: usize,
    t: &Theme,
) -> (Vec<Span<'static>>, Vec<(usize, usize, usize)>) {
    let widths: Vec<usize> = segs.iter().map(|s| Span::raw(&s.text).width()).collect();
    let (sep, collapsed) = (Span::raw(SEPARATOR).width(), Span::raw(COLLAPSED).width());
    let last = segs.len() - 1;
    let shown_width = |skip: usize| {
        let rest = widths[skip..].iter().sum::<usize>() + sep * (last - skip);
        if skip == 0 {
            rest
        } else {
            collapsed + sep + rest
        }
    };
    let skip = (0..last).find(|&k| shown_width(k) <= width).unwrap_or(last);
    let mut spans = Vec::new();
    let mut hits = Vec::new();
    let mut x = 0;
    if skip > 0 {
        spans.push(Span::styled(COLLAPSED, t.fg_dim));
        spans.push(Span::styled(SEPARATOR, t.fg_dim));
        x += collapsed + sep;
    }
    for (i, seg) in segs.iter().enumerate().skip(skip) {
        if i == last {
            let room = width.saturating_sub(x);
            spans.push(Span::styled(truncate_middle(&seg.text, room), t.title));
        } else {
            spans.push(Span::styled(seg.text.clone(), t.fg_dim));
            spans.push(Span::styled(SEPARATOR, t.fg_dim));
            hits.push((x, widths[i], i));
        }
        x += widths[i] + sep;
    }
    (spans, hits)
}

/// The right of the first line: whether the browser is read-only, and the scan's state. Longest
/// first, then the shorter forms for narrow terminals: `read-only · done in 00:16`,
/// `read-only · done 00:16`, `done 00:16`, `00:16`. While scanning, each keeps the spinner.
fn scan_states(app: &App, now: Instant, frame: u64) -> Vec<Vec<Span<'static>>> {
    let ((long, short, bare), style) = match app.done_at {
        Some(done) if !app.snap.busy => {
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

/// Between two modes on the right of the second line, on wide terminals and on narrow ones.
const WIDE_SEPARATOR: &str = "  ·  ";
const NARROW_SEPARATOR: &str = " · ";
/// The wide forms of the modes are used from this many columns, as in the mockups: the list
/// has its widest layout there too.
const WIDE_FROM: u16 = 110;
/// At most this many cells of a filter query or a type filter in the header.
const QUERY_MAX: usize = 16;

/// One mode on the right of the second line, in its form for wide terminals and its short one.
struct Flag {
    wide: Vec<Span<'static>>,
    narrow: Vec<Span<'static>>,
}

impl Flag {
    /// A mode written `wide`, or `narrow` when the line is short.
    fn new(wide: impl Into<String>, narrow: impl Into<String>) -> Flag {
        Flag {
            wide: vec![Span::raw(wide.into())],
            narrow: vec![Span::raw(narrow.into())],
        }
    }
}

/// Tree mode, shown before the sort while the rows are a tree.
fn tree_flag(app: &App) -> Option<Flag> {
    app.tree_shown()?;
    let tree = vec![Span::styled("tree", app.theme.key)];
    Some(Flag {
        wide: tree.clone(),
        narrow: tree,
    })
}

/// The sort, always shown: `sort size ↓`, or `size ↓` when the line is short.
fn sort_flag(app: &App) -> Flag {
    let key = match app.sort.key {
        SortKey::Size => "size",
        SortKey::Name => "name",
        SortKey::Items => "items",
        SortKey::Mtime => "mtime",
    };
    let arrow = if app.sort.desc { "↓" } else { "↑" };
    let sort = format!("{key} {arrow}");
    Flag {
        wide: vec![
            Span::styled("sort ", app.theme.fg_dim),
            Span::raw(sort.clone()),
        ],
        narrow: vec![Span::raw(sort)],
    }
}

/// The modes after the sort, in the order they give way when the line is short (the last
/// first): in tree mode that the share is of the parent and what folds, the size shown, then
/// only what differs from the defaults:
/// hidden files hidden, the filter kept, the type filter, and the day of the last recorded scan
/// the changes are from.
fn flags(app: &App) -> Vec<Flag> {
    let mut out = Vec::new();
    if let Some(tree) = app.tree_shown() {
        out.push(Flag::new("share of parent", "of parent"));
        if let Some(fold) = crate::state::tree::act::fold_label(tree.fold_ratio) {
            out.push(Flag::new(fold, fold));
        }
    }
    out.push(match app.size_mode {
        SizeMode::Disk => Flag::new("disk usage", "disk"),
        SizeMode::Apparent => Flag::new("apparent size", "apparent"),
        SizeMode::Referenced => Flag::new("disk usage like du", "du-like"),
    });
    if app.hide_hidden {
        out.push(Flag::new("hiding hidden files", "hidden"));
    }
    if app.filter.active() && !filter::editing(app) {
        let query = format!("/{}", truncate_middle(app.filter.query(), QUERY_MAX));
        out.push(Flag::new(query.clone(), query));
    }
    if !app.snap.types.is_empty() {
        let types = truncate_middle(&app.snap.types.to_string(), QUERY_MAX);
        out.push(Flag::new(format!("type {types}"), format!("t {types}")));
    }
    // The size `du` counts is not recorded, so there are no changes to show then.
    if let Some(g) = &app.growth
        && app.size_mode != SizeMode::Referenced
    {
        out.push(Flag::new(
            format!("Δ since {}", history::day(g.at)),
            growth::column_title(app),
        ));
    }
    out
}

/// The right of the second line in the forms to try, longest first: every mode in its wide
/// form (used from [`WIDE_FROM`] columns), then every mode short, then short with the last
/// modes left out and `+N` in their place. Tree mode and the sort are never left out.
fn mode_forms(app: &App) -> Vec<Vec<Span<'static>>> {
    let t = &app.theme;
    let lead: Vec<Flag> = tree_flag(app).into_iter().chain([sort_flag(app)]).collect();
    let rest = flags(app);
    let join = |parts: Vec<Vec<Span<'static>>>, sep: &'static str| {
        let mut spans = Vec::new();
        for (i, part) in parts.into_iter().enumerate() {
            if i > 0 {
                spans.push(Span::styled(sep, t.fg_dim));
            }
            spans.extend(part);
        }
        spans.push(Span::raw(" "));
        spans
    };
    let wide = lead.iter().chain(&rest).map(|f| f.wide.clone()).collect();
    let mut forms = vec![join(wide, WIDE_SEPARATOR)];
    for kept in (0..=rest.len()).rev() {
        let mut parts: Vec<_> = lead
            .iter()
            .chain(&rest[..kept])
            .map(|f| f.narrow.clone())
            .collect();
        let dropped = rest.len() - kept;
        if dropped > 0 {
            parts.push(vec![Span::styled(format!("+{dropped}"), t.fg_dim)]);
        }
        forms.push(join(parts, NARROW_SEPARATOR));
    }
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
    let t = &app.theme;
    let s = &app.snap;
    let w = usize::from(area.width);

    // The tab bar takes its fullest form that fits beside a form of the scan state, which
    // shortens first; one cell stays free between them. The bare time, which no longer says
    // whether the scan is done, comes only after every form of the tab bar.
    let mut left = vec![Span::styled(" dfdu ", t.title)];
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
    let line1 = spread(left, state.clone(), area.width);
    let tab_hits: Vec<Tab> = (bar.hits.iter())
        .filter(|&&(x, tw, _)| area.height > 0 && start + x + tw <= w)
        .map(|&(x, tw, view)| Tab {
            area: Rect::new(area.x + cells(start + x), area.y, cells(tw), 1),
            view,
        })
        .collect();

    // The scope keeps the current directory's name whole, up to [`SCOPE_KEPT`] cells of it (a
    // long name or scan root is shortened in the middle rather than hide the global state),
    // or a few cells of the scan root: the modes give way first, in their longest form that
    // leaves it room.
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
    // Then the scope shortens, as the sort is never left out while the line has room for the
    // totals and it.
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
    let right2 = (rights.iter())
        .find(|right| room(right) >= least)
        .or_else(|| rights.last().filter(|r| lead + width(r) < w))
        .cloned()
        .unwrap_or_else(|| {
            let mut right = info.clone();
            right.push(Span::raw(" "));
            right
        });
    let scope_room = room(&right2);
    let mut scope = vec![Span::raw(" ")];
    let mut crumb_hits = Vec::new();
    if whole_scan {
        // Without room for a few cells of the root, only that it is the whole scan.
        let rest = scope_room.saturating_sub(Span::raw(WHOLE_SCAN).width());
        if !root.is_empty() && rest >= Span::raw(&root).width().min(WHOLE_SCAN_ROOT_MIN) {
            scope.push(Span::styled(WHOLE_SCAN, t.fg_dim));
            scope.push(Span::styled(truncate_middle(&root, rest), t.title));
            // What the view covers of it, when that fits whole.
            let note = views::scope_note(view).map(|n| format!(" · {n}"));
            let left = rest.saturating_sub(Span::raw(&root).width());
            if let Some(note) = note.filter(|n| Span::raw(n).width() <= left) {
                scope.push(Span::styled(note, t.fg_dim));
            }
        } else {
            let short = truncate_middle(WHOLE_SCAN.trim_end_matches([':', ' ']), scope_room);
            scope.push(Span::styled(short, t.fg_dim));
        }
    } else {
        let (crumbs, hits) = breadcrumbs(&segs, scope_room, t);
        scope.extend(crumbs);
        crumb_hits = hits;
    }
    let line2 = spread(scope, right2, area.width);
    f.render_widget(Paragraph::new(vec![line1, line2]), area);
    app.mouse.tabs = tab_hits;
    app.mouse.crumbs = crumb_hits
        .into_iter()
        .filter(|&(x, cw, _)| area.height > 1 && lead + x + cw <= w)
        .map(|(x, cw, i)| Crumb {
            area: Rect::new(area.x + cells(lead + x), area.y + 1, cells(cw), 1),
            child: segs[i + 1].dir.clone(),
        })
        .collect();
}

#[cfg(test)]
mod tests;
