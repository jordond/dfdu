//! The two header lines: breadcrumbs from the scan root to the current directory and the scan
//! state, then totals and the modes that change the numbers: the sort, the size shown, and what
//! differs from the defaults.

use std::path::{Path, PathBuf};
use std::time::Instant;

use dfdu_engine::SizeMode;
use dfdu_store::history;
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::text::Span;
use ratatui::widgets::Paragraph;

use super::{cells, mmss, spread};
use crate::app::App;
use crate::input::mouse::Crumb;
use crate::model::growth;
use crate::state::filter;
use crate::state::order::SortKey;
use crate::theme::{Theme, count, human, spinner, truncate_middle};

/// Between two breadcrumbs.
const SEPARATOR: &str = " › ";
/// In place of the leading breadcrumbs that do not fit.
const COLLAPSED: &str = "…";

/// One breadcrumb: a directory on the way from the scan root to the current one.
#[derive(Debug, PartialEq)]
struct Segment {
    text: String,
    dir: PathBuf,
}

/// The breadcrumbs of `dir`: the scan root `root` as its path, with the home directory `home`
/// shown as `~`, then one per directory below it. All of `dir` is one breadcrumb when it is not
/// below `root` (before the first snapshot).
fn segments(dir: &Path, root: &Path, home: Option<&Path>) -> Vec<Segment> {
    let (root, below) = match dir.strip_prefix(root) {
        Ok(below) if !root.as_os_str().is_empty() => (root, below),
        _ => (dir, Path::new("")),
    };
    // A home directory of `/` would turn every path into `~/...`.
    let home = home.filter(|h| h.parent().is_some());
    let text = match home.and_then(|h| root.strip_prefix(h).ok()) {
        Some(rest) if rest.as_os_str().is_empty() => "~".to_owned(),
        Some(rest) => Path::new("~").join(rest).display().to_string(),
        None => root.display().to_string(),
    };
    let mut out = vec![Segment {
        text,
        dir: root.to_path_buf(),
    }];
    let mut dir = root.to_path_buf();
    for part in below.components() {
        dir.push(part);
        out.push(Segment {
            text: part.as_os_str().to_string_lossy().into_owned(),
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
    let (long, short, bare) = match app.done_at {
        Some(done) if !app.snap.busy => {
            let time = mmss(done.duration_since(app.started));
            (format!("done in {time}"), format!("done {time}"), time)
        }
        _ => {
            let (spin, time) = (spinner(frame), mmss(now.duration_since(app.started)));
            let scanning = format!("{spin} scanning {time}");
            (scanning.clone(), scanning, format!("{spin} {time}"))
        }
    };
    let state = |text: &str| vec![Span::raw(text.to_owned()), Span::raw(" ")];
    let read_only = |text: &str| {
        let mut spans = vec![
            Span::styled("read-only", app.theme.status_warn),
            Span::raw(" · "),
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

/// Tree mode, shown before the sort when it is on. Tree mode is not built yet, so there is
/// nothing to show; this is where its flag goes.
fn tree_flag(_app: &App) -> Option<Flag> {
    None
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
/// first): the size shown, the treemap's colouring and depth while it is shown, then only what
/// differs from the defaults: hidden files hidden, the filter kept, the type filter, and the day
/// of the last recorded scan the changes are from.
fn flags(app: &App) -> Vec<Flag> {
    let mut out = vec![match app.size_mode {
        SizeMode::Disk => Flag::new("disk usage", "disk"),
        SizeMode::Apparent => Flag::new("apparent size", "apparent"),
        SizeMode::Referenced => Flag::new("disk usage like du", "du-like"),
    }];
    if app.treemap.shown {
        let colour = app.treemap.color.label();
        out.push(Flag::new(format!("colour by {colour}"), colour));
        let depth = format!("depth {}", app.treemap.depth);
        out.push(Flag::new(depth.clone(), depth));
    }
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

/// Draws the header in `area` and records where each breadcrumb went, for the mouse.
pub fn draw(f: &mut Frame, app: &mut App, area: Rect, now: Instant, frame: u64) {
    let t = &app.theme;
    let s = &app.snap;
    let mut left = vec![Span::styled(" dfdu ", t.title), Span::raw(" ")];
    let start: usize = left.iter().map(Span::width).sum();
    let segs = segments(&s.dir_path, &s.root_path, app.home.as_deref());
    // One cell stays free between the breadcrumbs and the scan state. The state takes its
    // longest form that leaves the current directory's name whole.
    let room = |right: &[Span]| {
        let end: usize = right.iter().map(Span::width).sum();
        usize::from(area.width).saturating_sub(start + end + 1)
    };
    let mut forms = scan_states(app, now, frame);
    let fits = forms.iter().position(|r| room(r) >= least_width(&segs));
    let right = forms.swap_remove(fits.unwrap_or(forms.len() - 1));
    let crumb_room = room(&right);
    let (crumbs, hits) = breadcrumbs(&segs, crumb_room, t);
    left.extend(crumbs);
    let line1 = spread(left, right, area.width);

    let root = s.root_total;
    let size = app.size_mode.of(&root);
    let mut info = vec![Span::raw(format!(
        " {} items · {}",
        count(root.items),
        human(size)
    ))];
    if root.errors > 0 {
        info.push(Span::raw(" · "));
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
    // The modes take their longest form that leaves a cell free after the totals, or give way.
    let used: usize = info.iter().map(Span::width).sum();
    let right2 = mode_forms(app)
        .into_iter()
        .skip(usize::from(area.width < WIDE_FROM))
        .find(|r| used + 1 + r.iter().map(Span::width).sum::<usize>() <= usize::from(area.width))
        .unwrap_or_default();
    let line2 = spread(info, right2, area.width);
    f.render_widget(Paragraph::new(vec![line1, line2]), area);
    app.mouse.crumbs = hits
        .into_iter()
        .filter(|&(x, w, _)| area.height > 0 && start + x + w <= area.width as usize)
        .map(|(x, w, i)| Crumb {
            area: Rect::new(area.x + cells(start + x), area.y, cells(w), 1),
            child: segs[i + 1].dir.clone(),
        })
        .collect();
}

#[cfg(test)]
mod tests;
