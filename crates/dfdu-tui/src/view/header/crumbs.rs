//! The breadcrumbs of the scope line: the scan root, with the home directory as `~`, and a
//! breadcrumb per directory below it down to the current one, the leading ones giving way to
//! `…` when they do not fit.

use std::path::Path;

use dfdu_tree::{Host, HostPath};
use ratatui::text::Span;

use crate::format::truncate_middle;
use crate::model::outside::rel_text;
use crate::theme::Theme;

/// Between two breadcrumbs.
const SEPARATOR: &str = " › ";
/// In place of the leading breadcrumbs that do not fit.
const COLLAPSED: &str = "…";

/// One breadcrumb: a directory on the way from the scan root to the current one.
#[derive(Debug, PartialEq)]
pub(super) struct Segment {
    pub(super) text: String,
    pub(super) dir: HostPath,
}

/// The breadcrumbs of `dir`: the scan root `root` as its path, with the home directory `home`
/// shown as `~`, then one per directory below it. All of `dir` is one breadcrumb when it is not
/// below `root` (before the first snapshot). `home` is this machine's: another host's root is
/// shown as it is.
pub(super) fn segments(dir: &HostPath, root: &HostPath, home: Option<&Path>) -> Vec<Segment> {
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
pub(super) fn least_width(segs: &[Segment]) -> usize {
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
pub(super) fn breadcrumbs(
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

#[cfg(test)]
mod tests {
    use super::*;

    fn texts(dir: &str, root: &str, home: Option<&str>) -> Vec<String> {
        segments(&dir.into(), &root.into(), home.map(Path::new))
            .into_iter()
            .map(|s| s.text)
            .collect()
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
    fn leading_breadcrumbs_give_way_to_an_ellipsis() {
        let segs = segments(&"/srv/data/dev/rust".into(), &"/srv/data".into(), None);
        let t = Theme::no_color();
        let shown = |width| {
            let (spans, hits) = breadcrumbs(&segs, width, &t);
            let text: String = spans.iter().map(|s| s.content.as_ref()).collect();
            (text, hits)
        };
        assert_eq!(
            shown(80),
            ("/srv/data › dev › rust".into(), vec![(0, 9, 0), (12, 3, 1)])
        );
        assert_eq!(shown(16), ("… › dev › rust".into(), vec![(4, 3, 1)]));
        assert_eq!(least_width(&segs), "… › rust".chars().count());
        // The last one is always shown, shortened when it does not fit by itself.
        let (text, hits) = shown(5);
        assert!(text.starts_with("… › ") && text.ends_with('…'), "{text}");
        assert_eq!(hits, []);
    }
}
