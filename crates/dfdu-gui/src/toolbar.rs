//! The toolbar: what the list shows (disk usage or apparent size, hidden files, tree mode), the
//! filter, and the scan's state with a spinner on the first line; the tabs of the views and the
//! breadcrumbs from the scan root to the current directory on the second.

use std::path::Path;
use std::time::{Duration, Instant};

use dfdu_engine::SizeMode;
use dfdu_session::SessionState;
use dfdu_session::actions::{Action, first_key};
use dfdu_session::palette::Token;
use dfdu_session::state::Cached;
use dfdu_session::view::View;
use dfdu_tree::HostPath;
use egui::{Id, Key, Sense, Shape, Stroke, TextEdit, Ui, vec2};

use crate::browser::{Intent, ViewState};
use crate::theme::Colors;

/// Between two breadcrumbs.
const SEPARATOR: &str = "›";
/// The filter field's id, to tell whether it has the keyboard.
const FILTER: &str = "dfdu-filter";
/// The views the window shows, in the order of their tabs.
pub const VIEWS: &[View] = &View::ALL;

pub fn show(
    ui: &mut Ui,
    s: &SessionState,
    colors: &Colors,
    view: &mut ViewState,
    now: Instant,
    out: &mut Vec<Intent>,
) {
    ui.horizontal(|ui| {
        size_mode(ui, s, out);
        ui.separator();
        let mut hidden = !s.hide_hidden;
        if ui.checkbox(&mut hidden, "Hidden files").changed() {
            out.push(Intent::Act(Action::ToggleHidden));
        }
        let mut tree = s.tree.is_some();
        if ui.checkbox(&mut tree, "Tree").changed() {
            out.push(Intent::Act(Action::ToggleTree));
        }
        let mut details = s.details.open;
        if ui.checkbox(&mut details, "Details").changed() {
            out.push(Intent::Act(Action::Details));
        }
        ui.separator();
        filter(ui, s, view, out);
        types(ui, s, colors, view, out);
        if ui.button("Rescan").clicked() {
            out.push(Intent::Act(Action::Rescan));
        }
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            scan_state(ui, s, colors, now);
        });
    });
    ui.horizontal_wrapped(|ui| {
        tabs(ui, s, out);
        ui.separator();
        breadcrumbs(ui, s, colors, out);
    });
}

/// The tabs of the views, each switching to its view as its key does.
fn tabs(ui: &mut Ui, s: &SessionState, out: &mut Vec<Intent>) {
    let current = s.current_view();
    for &v in VIEWS {
        let r = ui.selectable_label(current == v, v.name());
        if r.on_hover_text(format!("key {}", first_key(v.action())))
            .clicked()
            && current != v
        {
            out.push(Intent::Act(v.action()));
        }
    }
}

/// Disk usage or apparent size, as `a` switches them.
fn size_mode(ui: &mut Ui, s: &SessionState, out: &mut Vec<Intent>) {
    for (mode, label) in [
        (SizeMode::Disk, "Disk usage"),
        (SizeMode::Apparent, "Apparent size"),
    ] {
        let shown = s.size_mode == mode;
        if ui.selectable_label(shown, label).clicked() && !shown {
            out.push(Intent::Act(Action::ToggleApparent));
        }
    }
}

/// The filter field: typing filters the list at once, `Enter` keeps the filter and gives the
/// keys back to the list, `Esc` clears it.
fn filter(ui: &mut Ui, s: &SessionState, view: &mut ViewState, out: &mut Vec<Intent>) {
    let id = Id::unique(FILTER);
    if !ui.memory(|m| m.has_focus(id)) {
        s.filter.input().text().clone_into(&mut view.filter_text);
    }
    let field = TextEdit::singleline(&mut view.filter_text)
        .id(id)
        .hint_text("Filter (/)")
        .desired_width(ui.spacing().text_edit_width);
    let r = ui.add(field);
    if std::mem::take(&mut view.focus_filter) {
        r.request_focus();
    }
    if r.changed() {
        out.push(Intent::Filter(view.filter_text.clone()));
    }
    if r.lost_focus() {
        if ui.input(|i| i.key_pressed(Key::Enter)) {
            out.push(Intent::FilterSubmit);
        } else if ui.input(|i| i.key_pressed(Key::Escape)) {
            out.push(Intent::FilterCancel);
        }
    }
    if s.filter.active() {
        let (shown, all) = s.filter.shown(s.hide_hidden);
        ui.label(format!("{shown} of {all}"));
    }
}

/// The type picker's button, which says which types are shown while the filter is on; and a
/// button that turns the filter off.
fn types(ui: &mut Ui, s: &SessionState, colors: &Colors, view: &ViewState, out: &mut Vec<Intent>) {
    let label = if s.types_active() {
        colors.text(format!("Only {}", s.types), Token::StatusWarn)
    } else {
        egui::RichText::new("File types…")
    };
    if ui.selectable_label(view.types.is_some(), label).clicked() {
        out.push(Intent::Act(Action::Types));
    }
    if s.types_active() && ui.button("All files").clicked() {
        out.push(Intent::Type(dfdu_session::type_filter::Row::All));
    }
}

/// `00:16`.
pub fn mmss(d: Duration) -> String {
    let s = d.as_secs();
    format!("{:02}:{:02}", s / 60, s % 60)
}

/// The scan's state, right to left: the time and what the scan does, a spinner while it runs,
/// and whether the browser is read-only.
fn scan_state(ui: &mut Ui, s: &SessionState, colors: &Colors, now: Instant) {
    let elapsed = mmss(now.duration_since(s.started));
    let (text, token) = match (s.cached, s.done_at) {
        (Some(Cached::Loading), _) => (format!("loading the cached scan {elapsed}"), None),
        (Some(Cached::Refreshing { written }), _) => {
            let age = dfdu_session::cache_link::ago(
                written,
                dfdu_tree::now_secs().max(0).cast_unsigned(),
            );
            (format!("cached {age} ago, refreshing {elapsed}"), None)
        }
        (None, Some(done)) if !s.snap.busy => {
            let took = mmss(done.duration_since(s.started));
            (format!("done in {took}"), Some(Token::ScanDone))
        }
        _ => (format!("scanning {elapsed}"), None),
    };
    let running = token.is_none();
    ui.label(colors.text(text, token.unwrap_or(Token::ScanActive)));
    if running {
        spinner(
            ui,
            colors.fg(Token::ScanActive),
            now.duration_since(s.started),
        );
    }
    if s.read_only {
        ui.label(colors.text("read-only", Token::StatusWarn));
    }
}

/// A spinner turned by `elapsed`. Unlike egui's, it asks for no frames: the session asks for
/// one each time a spinner is due to move.
fn spinner(ui: &mut Ui, color: egui::Color32, elapsed: Duration) {
    let size = ui.spacing().interact_size.y;
    let (rect, response) = ui.allocate_exact_size(vec2(size, size), Sense::hover());
    response.widget_info(|| egui::WidgetInfo::new(egui::Role::ProgressIndicator));
    let radius = rect.height() * 0.5 - 2.0;
    let start = elapsed.as_secs_f64() * std::f64::consts::TAU;
    let points = (0..=16)
        .map(|i| {
            let angle = start + f64::from(i) / 16.0 * std::f64::consts::PI * 1.4;
            let (sin, cos) = angle.sin_cos();
            #[expect(clippy::cast_possible_truncation, reason = "a point on screen")]
            let v = vec2(cos as f32, sin as f32);
            rect.center() + radius * v
        })
        .collect();
    ui.painter()
        .add(Shape::line(points, Stroke::new(2.0, color)));
}

/// One breadcrumb: a directory on the way from the scan root to the current one.
#[derive(Debug, PartialEq)]
pub struct Segment {
    pub text: String,
    pub dir: HostPath,
}

/// The breadcrumbs of `dir`, as the terminal shows them: the scan root `root` as its path, with
/// the home directory `home` shown as `~`, then one per directory below it. All of `dir` is one
/// breadcrumb when it is not below `root` (before the first snapshot).
pub fn segments(dir: &HostPath, root: &HostPath, home: Option<&Path>) -> Vec<Segment> {
    let (root, below) = match dir.rel(root) {
        Some(below) if !root.is_empty() => (root, below),
        _ => (dir, Vec::new()),
    };
    let text = dfdu_engine::tilde(root, home);
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

/// The breadcrumbs; a click on one but the last opens its directory.
fn breadcrumbs(ui: &mut Ui, s: &SessionState, colors: &Colors, out: &mut Vec<Intent>) {
    let snap = &s.snap;
    let segs = segments(&snap.dir_path, &snap.root_path, s.home.as_deref());
    let last = segs.len() - 1;
    for (i, seg) in segs.iter().enumerate() {
        if i == last {
            ui.label(colors.text(&seg.text, Token::Title));
            break;
        }
        if ui.link(colors.text(&seg.text, Token::Dir)).clicked() {
            out.push(Intent::GoTo {
                child: segs[i + 1].dir.clone(),
            });
        }
        ui.label(colors.text(SEPARATOR, Token::FgDim));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn breadcrumbs_run_from_the_root_with_home_as_tilde() {
        let home = Path::new("/home/u");
        let root = HostPath::from("/home/u/dev");
        // Joined as the tree joins them: with `\` on Windows.
        let dir = root.join(b"a").join(b"b");
        let segs = segments(&dir, &root, Some(home));
        let texts: Vec<&str> = segs.iter().map(|s| s.text.as_str()).collect();
        assert_eq!(texts, ["~/dev", "a", "b"]);
        assert_eq!(segs[1].dir, root.join(b"a"));
        // Before the first snapshot the root is not known: the directory is one breadcrumb.
        let segs = segments(&dir, &HostPath::from(""), Some(home));
        assert_eq!(segs.len(), 1);
        assert_eq!(mmss(Duration::from_secs(75)), "01:15");
    }
}
