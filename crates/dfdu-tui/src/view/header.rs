//! The two header lines: breadcrumbs from the scan root to the current directory and the scan
//! state, then totals and the sort.

use std::path::{Path, PathBuf};
use std::time::Instant;

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::text::Span;
use ratatui::widgets::Paragraph;

use super::{cells, mmss, spread};
use crate::app::App;
use crate::input::mouse::Crumb;
use crate::state::order::SortKey;
use crate::theme::{Theme, count, human, spinner};

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

/// Lays out `segs` in at most `width` cells, putting `…` in place of leading breadcrumbs until
/// the rest fits; the last one is always shown. Returns the spans, and the column offset, width
/// and index in `segs` of each breadcrumb shown before the last.
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
            spans.push(Span::styled(seg.text.clone(), t.title));
        } else {
            spans.push(Span::styled(seg.text.clone(), t.fg_dim));
            spans.push(Span::styled(SEPARATOR, t.fg_dim));
            hits.push((x, widths[i], i));
        }
        x += widths[i] + sep;
    }
    (spans, hits)
}

/// The right of the first line: whether the browser is read-only, and the scan's state.
fn scan_state(app: &App, now: Instant, frame: u64) -> Vec<Span<'static>> {
    let state = match app.done_at {
        Some(done) if !app.snap.busy => {
            format!("done in {}", mmss(done.duration_since(app.started)))
        }
        _ => format!(
            "{} scanning {}",
            spinner(frame),
            mmss(now.duration_since(app.started))
        ),
    };
    let mut spans = Vec::new();
    if app.read_only {
        spans.push(Span::styled("read-only", app.theme.status_warn));
        spans.push(Span::raw(" · "));
    }
    spans.extend([Span::raw(state), Span::raw(" ")]);
    spans
}

/// Draws the header in `area` and records where each breadcrumb went, for the mouse.
pub fn draw(f: &mut Frame, app: &mut App, area: Rect, now: Instant, frame: u64) {
    let t = &app.theme;
    let s = &app.snap;
    let mut left = vec![Span::styled(" dfdu ", t.title), Span::raw(" ")];
    let right = scan_state(app, now, frame);
    let start: usize = left.iter().map(Span::width).sum();
    let end: usize = right.iter().map(Span::width).sum();
    // One cell stays free between the breadcrumbs and the scan state.
    let crumb_room = usize::from(area.width).saturating_sub(start + end + 1);
    let segs = segments(&s.dir_path, &s.root_path, app.home.as_deref());
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
    let key = match app.sort.key {
        SortKey::Size => "size",
        SortKey::Name => "name",
        SortKey::Items => "items",
        SortKey::Mtime => "mtime",
    };
    let arrow = if app.sort.desc { "↓" } else { "↑" };
    let mode = app.size_mode.label();
    let right2 = vec![Span::styled(
        format!("sort: {key} {arrow}  {mode} "),
        t.fg_dim,
    )];
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
mod tests {
    use ratatui::buffer::Buffer;

    use super::*;
    use crate::model::snapshot::Snapshot;
    use crate::tests::{render_buffer, snap_without_totals, style_at, text};

    fn texts(dir: &str, root: &str, home: Option<&str>) -> Vec<String> {
        segments(Path::new(dir), Path::new(root), home.map(Path::new))
            .into_iter()
            .map(|s| s.text)
            .collect()
    }

    /// A browser of `dir` below the scan root `root`, drawn `width` columns wide.
    fn browser(dir: &str, root: &str, theme: Theme, width: u16) -> (App, Buffer) {
        let t0 = Instant::now();
        let mut app = App::new(theme, t0);
        let below = Snapshot {
            dir_path: dir.into(),
            root_path: root.into(),
            at_root: dir == root,
            ..snap_without_totals(Vec::new(), 1)
        };
        app.refresh(below, t0);
        let buf = render_buffer(&mut app, t0, width, 8);
        (app, buf)
    }

    fn first_line(buf: &Buffer) -> String {
        text(buf).lines().next().unwrap_or_default().to_owned()
    }

    #[test]
    fn breadcrumbs_show_path_below_root() {
        assert_eq!(
            texts("/srv/data/dev/rust", "/srv/data", None),
            ["/srv/data", "dev", "rust"]
        );
        assert_eq!(texts("/srv/data", "/srv/data", None), ["/srv/data"]);
        // Before the first snapshot nothing is known about the root.
        assert_eq!(texts("/srv/data/dev", "", None), ["/srv/data/dev"]);

        let (app, buf) = browser("/srv/data/dev/rust", "/srv/data", Theme::ansi16(), 100);
        let first = first_line(&buf);
        assert!(
            first.starts_with(" dfdu  /srv/data › dev › rust "),
            "{first}"
        );
        // The current directory is styled as the title, the way to it dimmed.
        assert_eq!(style_at(&buf, "rust"), style_at(&buf, "dfdu"));
        assert_eq!(style_at(&buf, "dev"), style_at(&buf, "›"));
        assert_ne!(style_at(&buf, "dev"), style_at(&buf, "rust"));
        let children: Vec<_> = app.mouse.crumbs.iter().map(|c| c.child.clone()).collect();
        assert_eq!(
            children,
            ["/srv/data/dev", "/srv/data/dev/rust"].map(PathBuf::from)
        );
    }

    #[test]
    fn long_paths_collapse_from_the_left() {
        let dir = "/srv/data/projects/rust/dfdu/src/ui";
        let (app, buf) = browser(dir, "/srv/data", Theme::no_color(), 40);
        let header: String = text(&buf).lines().take(2).flat_map(|l| [l, "\n"]).collect();
        insta::assert_snapshot!("breadcrumbs_collapsed", header);
        assert!(header.starts_with(" dfdu  … › src › ui "), "{header}");
        let crumbs: Vec<_> = app
            .mouse
            .crumbs
            .iter()
            .map(|c| (c.area, c.child.clone()))
            .collect();
        assert_eq!(crumbs, [(Rect::new(11, 0, 3, 1), PathBuf::from(dir))]);

        let (_, buf) = browser(dir, "/srv/data", Theme::no_color(), 100);
        let first = first_line(&buf);
        assert!(
            first.starts_with(" dfdu  /srv/data › projects › rust › dfdu › src › ui "),
            "{first}"
        );
    }

    #[test]
    fn home_is_shown_as_tilde() {
        let home = Some("/home/jo");
        assert_eq!(
            texts("/home/jo/dev/rust", "/home/jo", home),
            ["~", "dev", "rust"]
        );
        assert_eq!(
            texts("/home/jo/dev/rust", "/home/jo/dev", home),
            // Joined as paths are, with `\` on Windows.
            [
                Path::new("~").join("dev").display().to_string(),
                "rust".into()
            ]
        );
        assert_eq!(
            texts("/home/joe/dev", "/home/joe", home),
            ["/home/joe", "dev"]
        );
        assert_eq!(texts("/srv", "/srv", home), ["/srv"]);
        // A home directory of `/` is not shown as `~`.
        assert_eq!(texts("/srv/a", "/srv", Some("/")), ["/srv", "a"]);
    }

    #[test]
    fn header_shows_the_home_directory_as_tilde() {
        let t0 = Instant::now();
        let mut app = App::new(Theme::no_color(), t0);
        let below = Snapshot {
            dir_path: "/home/jo/dev".into(),
            root_path: "/home/jo".into(),
            at_root: false,
            ..snap_without_totals(Vec::new(), 1)
        };
        app.refresh(below, t0);
        let first = first_line(&render_buffer(&mut app, t0, 80, 8));
        assert!(first.starts_with(" dfdu  /home/jo › dev "), "{first}");

        app.home = Some("/home/jo".into());
        let first = first_line(&render_buffer(&mut app, t0, 80, 8));
        assert!(first.starts_with(" dfdu  ~ › dev "), "{first}");
        let crumbs: Vec<_> = app.mouse.crumbs.iter().map(|c| c.child.clone()).collect();
        assert_eq!(crumbs, [PathBuf::from("/home/jo/dev")]);
    }
}
