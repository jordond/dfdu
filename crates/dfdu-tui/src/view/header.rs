//! The two header lines: the current directory and scan state, then totals and the sort.

use std::time::Instant;

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::text::Span;
use ratatui::widgets::Paragraph;

use super::{mmss, spread};
use crate::app::App;
use crate::order::SortKey;
use crate::theme::{count, human, spinner};

pub fn draw(f: &mut Frame, app: &App, area: Rect, now: Instant, frame: u64) {
    let t = &app.theme;
    let s = &app.snap;
    let state = match app.done_at {
        Some(done) if !s.busy => format!("done in {}", mmss(done.duration_since(app.started))),
        _ => format!(
            "{} scanning {}",
            spinner(frame),
            mmss(now.duration_since(app.started))
        ),
    };
    let path = s.dir_path.display().to_string();
    let left = vec![
        Span::styled(" dfdu ", t.title),
        Span::raw(" "),
        Span::styled(path, t.title),
    ];
    let right = vec![Span::raw(state), Span::raw(" ")];
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
            t.error,
        ));
    }
    if s.incomplete {
        info.push(Span::styled(" · incomplete", t.warn));
    }
    if let Some((done, requested)) = s.measuring {
        let pct = (done * 100).checked_div(requested).unwrap_or(0);
        info.push(Span::styled(
            format!(" · measuring shared data {pct}%"),
            t.dim,
        ));
    }
    if s.unmeasured > 0 {
        info.push(Span::styled(
            format!(" · {} files not measured", count(s.unmeasured)),
            t.warn,
        ));
    }
    if let Some((name, ratio)) = s.profile
        && ratio != 1.0
    {
        info.push(Span::styled(
            format!(" · data stored {ratio:.1}× ({name})"),
            t.dim,
        ));
    }
    if s.compressed_upper_bound {
        info.push(Span::styled(
            " · compressed sizes are upper bounds (exact as root)",
            t.dim,
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
    let right2 = vec![Span::styled(format!("sort: {key} {arrow}  {mode} "), t.dim)];
    let line2 = spread(info, right2, area.width);
    f.render_widget(Paragraph::new(vec![line1, line2]), area);
}
