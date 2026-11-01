//! The toolbar and the tab bar. The toolbar: the wordmark, the breadcrumbs from the scan root
//! to the current directory, the scan's state and totals, disk usage or apparent size, the
//! filter, and the View menu (theme, keys, palette). The tab bar: the tabs of the views (with
//! Cleanup's reclaimable total and the number of groups of duplicates), and on its right what
//! the list shows (tree mode, hidden files, the details sheet, file types) and Rescan.

use std::path::Path;
use std::time::{Duration, Instant};

use dfdu_engine::SizeMode;
use dfdu_session::actions::{Action, first_key};
use dfdu_session::format::{count, human, plural};
use dfdu_session::palette::Token;
use dfdu_session::state::Cached;
use dfdu_session::view::View;
use dfdu_session::{SessionState, cleanup, dupes};
use dfdu_tree::HostPath;
use egui::{
    Align, CornerRadius, Id, Key, Layout, Rect, Sense, Shape, Stroke, StrokeKind, TextEdit, Ui,
    WidgetInfo, vec2,
};

use crate::browser::{Intent, ViewState};
use crate::kit;
use crate::theme::Colors;
use crate::theme::style::{self, RADIUS_MD};

/// Between two breadcrumbs.
const SEPARATOR: &str = "/";
/// The filter field's id, to tell whether it has the keyboard.
const FILTER: &str = "dfdu-filter";
/// The views the window shows, in the order of their tabs.
pub const VIEWS: &[View] = &View::ALL;

/// The toolbar, 48 high on the window's background.
pub fn show(
    ui: &mut Ui,
    s: &SessionState,
    colors: &Colors,
    view: &mut ViewState,
    now: Instant,
    out: &mut Vec<Intent>,
) {
    ui.horizontal_centered(|ui| {
        ui.spacing_mut().item_spacing.x = 12.0;
        ui.label(kit::mono_bold("dfdu", 18.0).color(colors.fg(Token::Header)));
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            ui.spacing_mut().item_spacing.x = 12.0;
            menu(ui, s, view, out);
            filter(ui, s, colors, view, out);
            size_mode(ui, s, colors, out);
            scan_state(ui, s, colors, now, out);
            // What is left of the row, less a gap before the scan's state.
            let mut room = ui.available_rect_before_wrap();
            room.max.x -= 12.0;
            ui.scope_builder(
                egui::UiBuilder::new()
                    .max_rect(room)
                    .layout(Layout::left_to_right(Align::Center)),
                |ui| {
                    ui.set_clip_rect(room.intersect(ui.clip_rect()));
                    breadcrumbs(ui, s, colors, out);
                },
            );
        });
    });
}

/// The View menu: the theme, the keys and the command palette.
fn menu(ui: &mut Ui, s: &SessionState, view: &mut ViewState, out: &mut Vec<Intent>) {
    let _ = s;
    ui.menu_button(kit::label("View", 12.0), |ui| {
        if ui.button("Command palette…").clicked() {
            view.palette = Some(crate::palette::Palette::new());
            ui.close();
        }
        if ui.button("Keys").clicked() {
            view.help = Some(crate::help::Help::new());
            ui.close();
        }
        ui.separator();
        ui.label(kit::label("Theme", 11.0).weak());
        for (i, name) in view.themes.iter().enumerate() {
            if ui
                .radio(view.theme_current == Some(i), name.as_str())
                .clicked()
            {
                out.push(Intent::Theme(i));
            }
        }
    });
}

/// The tab bar, 32 high, with a rule in `border` under it.
pub fn tab_bar(
    ui: &mut Ui,
    s: &SessionState,
    colors: &Colors,
    view: &ViewState,
    out: &mut Vec<Intent>,
) {
    let full = ui.max_rect();
    ui.painter().hline(
        full.x_range(),
        full.bottom() - 0.5,
        Stroke::new(1.0, colors.fg(Token::Border)),
    );
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 0.0;
        tabs(ui, s, colors, out);
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            ui.spacing_mut().item_spacing.x = 4.0;
            options(ui, s, colors, view, out);
        });
    });
}

/// The tabs of the views, each switching to its view as its key does: its key in the key
/// colour, its name, and what the view found ([`badge`]); the current one underlined.
fn tabs(ui: &mut Ui, s: &SessionState, colors: &Colors, out: &mut Vec<Intent>) {
    let current = s.current_view();
    let h = ui.available_height();
    for (n, &v) in VIEWS.iter().enumerate() {
        let on = current == v;
        let name_color = if on {
            colors.fg(Token::Header)
        } else {
            colors.fg(Token::FgDim)
        };
        let extra = badge(s, v);
        let font = |family: &str, size: f32| {
            egui::FontId::new(size, egui::FontFamily::Name(family.into()))
        };
        let p = ui.painter();
        let gk = p.layout_no_wrap(
            (n + 1).to_string(),
            font(style::MONO_SEMIBOLD_FAMILY, 11.0),
            colors.fg(Token::Key),
        );
        let gn = p.layout_no_wrap(
            v.name().to_owned(),
            font(style::SANS_MEDIUM_FAMILY, 12.0),
            name_color,
        );
        let gb = extra.as_ref().map(|b| {
            p.layout_no_wrap(
                b.clone(),
                egui::FontId::new(12.0, egui::FontFamily::Monospace),
                colors.fg(Token::FgDim),
            )
        });
        let w = 12.0
            + gk.size().x
            + 6.0
            + gn.size().x
            + gb.as_ref().map_or(0.0, |g| 6.0 + g.size().x)
            + 12.0;
        let (rect, r) = ui.allocate_exact_size(vec2(w, h), Sense::click());
        let label = match &extra {
            Some(b) => format!("{} {b}", v.name()),
            None => v.name().to_owned(),
        };
        r.widget_info(|| WidgetInfo::selected(egui::Role::Tab, true, on, &label));
        let p = ui.painter();
        if r.hovered() && !on {
            p.rect_filled(
                rect.shrink2(vec2(2.0, 4.0)),
                CornerRadius::same(RADIUS_MD),
                colors.chrome.hover,
            );
        }
        let mut x = rect.left() + 12.0;
        let cy = rect.center().y;
        for g in [Some(gk), Some(gn), gb].into_iter().flatten() {
            let at = egui::pos2(x, cy - g.size().y / 2.0);
            x += g.size().x + 6.0;
            p.galley(at, g, colors.fg(Token::Fg));
        }
        if on {
            let line = Rect::from_min_max(
                egui::pos2(rect.left(), rect.bottom() - 2.0),
                rect.right_bottom(),
            );
            p.rect_filled(line, 0.0, colors.fg(Token::BorderFocus));
        }
        let r = r.on_hover_text(format!("key {}", first_key(v.action())));
        if r.clicked() && !on {
            out.push(Intent::Act(v.action()));
        }
    }
}

/// What the list shows, as toggles on the tab bar's right, and Rescan: right to left.
fn options(
    ui: &mut Ui,
    s: &SessionState,
    colors: &Colors,
    view: &ViewState,
    out: &mut Vec<Intent>,
) {
    if quiet(ui, colors, "Rescan", false, first_key(Action::Rescan)).clicked() {
        out.push(Intent::Act(Action::Rescan));
    }
    types(ui, s, colors, view, out);
    for (on, text, action) in [
        (s.details.open, "Details", Action::Details),
        (!s.hide_hidden, "Hidden files", Action::ToggleHidden),
        (s.tree.is_some(), "Tree", Action::ToggleTree),
    ] {
        if toggle(ui, colors, text, on, first_key(action)).clicked() {
            out.push(Intent::Act(action));
        }
    }
}

/// A frameless button of the tab bar, with its key as a tooltip.
fn quiet(ui: &mut Ui, colors: &Colors, text: &str, on: bool, key: &str) -> egui::Response {
    let color = if on {
        colors.fg(Token::Header)
    } else {
        colors.fg(Token::FgDim)
    };
    let b = egui::Button::new(kit::label(text, 12.0).color(color))
        .frame(false)
        .min_size(vec2(0.0, 24.0));
    ui.add(b).on_hover_text(format!("key {key}"))
}

/// A toggle of the tab bar: a check box in the accent when on.
fn toggle(ui: &mut Ui, colors: &Colors, text: &str, on: bool, key: &str) -> egui::Response {
    let font = egui::FontId::new(
        12.0,
        egui::FontFamily::Name(style::SANS_MEDIUM_FAMILY.into()),
    );
    let color = if on {
        colors.fg(Token::Fg)
    } else {
        colors.fg(Token::FgDim)
    };
    let g = ui.painter().layout_no_wrap(text.to_owned(), font, color);
    let box_side = 12.0;
    let w = 8.0 + box_side + 6.0 + g.size().x + 8.0;
    let (rect, r) = ui.allocate_exact_size(vec2(w, 24.0), Sense::click());
    r.widget_info(|| WidgetInfo::selected(egui::Role::CheckBox, true, on, text));
    let p = ui.painter();
    if r.hovered() {
        p.rect_filled(rect, CornerRadius::same(RADIUS_MD), colors.chrome.hover);
    }
    let b = Rect::from_center_size(
        egui::pos2(rect.left() + 8.0 + box_side / 2.0, rect.center().y),
        vec2(box_side, box_side),
    );
    if on {
        p.rect_filled(b, CornerRadius::same(3), colors.accent());
        let c = b.center();
        let tick = vec![
            egui::pos2(c.x - 3.0, c.y),
            egui::pos2(c.x - 0.8, c.y + 2.4),
            egui::pos2(c.x + 3.2, c.y - 2.6),
        ];
        p.add(Shape::line(tick, Stroke::new(1.6, colors.chrome.on_accent)));
    } else {
        p.rect_stroke(
            b,
            CornerRadius::same(3),
            Stroke::new(1.0, colors.fg(Token::Border)),
            StrokeKind::Inside,
        );
    }
    p.galley(
        egui::pos2(b.right() + 6.0, rect.center().y - g.size().y / 2.0),
        g,
        color,
    );
    r.on_hover_text(format!("key {key}"))
}

/// What a tab says after its name, as the terminal's do: Cleanup's reclaimable total once its
/// matches were counted, and the number of groups of duplicates once a comparison finished,
/// `…` while one runs.
pub fn badge(s: &SessionState, v: View) -> Option<String> {
    match v {
        View::Cleanup => cleanup::reclaimable(s).filter(|&b| b > 0).map(human),
        View::Duplicates => dupes::groups(s).map(|g| match g {
            dupes::Groups::Found(n) => count(n as u64),
            dupes::Groups::Running => "…".into(),
        }),
        View::Files | View::Map | View::Largest => None,
    }
}

/// Disk usage or apparent size, as `a` switches them.
fn size_mode(ui: &mut Ui, s: &SessionState, colors: &Colors, out: &mut Vec<Intent>) {
    let shown = usize::from(s.size_mode == SizeMode::Apparent);
    if kit::segmented(ui, colors, "Size mode", &["Disk", "Apparent"], shown).is_some() {
        out.push(Intent::Act(Action::ToggleApparent));
    }
}

/// The filter field, 220 wide with `/` in a key cap: typing filters the list at once, `Enter`
/// keeps the filter and gives the keys back to the list, `Esc` clears it.
fn filter(
    ui: &mut Ui,
    s: &SessionState,
    colors: &Colors,
    view: &mut ViewState,
    out: &mut Vec<Intent>,
) {
    let id = Id::unique(FILTER);
    if !ui.memory(|m| m.has_focus(id)) {
        s.filter.input().text().clone_into(&mut view.filter_text);
    }
    let focused = ui.memory(|m| m.has_focus(id));
    let (outer, _) = ui.allocate_exact_size(vec2(220.0, 28.0), Sense::hover());
    let edge = if focused {
        colors.fg(Token::BorderFocus)
    } else {
        colors.fg(Token::Border)
    };
    ui.painter()
        .rect_filled(outer, CornerRadius::same(RADIUS_MD), colors.chrome.raised);
    ui.painter().rect_stroke(
        outer,
        CornerRadius::same(RADIUS_MD),
        Stroke::new(if focused { 2.0 } else { 1.0 }, edge),
        StrokeKind::Inside,
    );
    let mut inner = ui.new_child(
        egui::UiBuilder::new()
            .max_rect(outer.shrink2(vec2(8.0, 0.0)))
            .layout(Layout::right_to_left(Align::Center)),
    );
    if s.filter.active() {
        let (shown, all) = s.filter.shown(s.hide_hidden);
        inner.label(kit::mono(format!("{shown}/{all}"), 11.0).color(colors.fg(Token::FgDim)));
    } else {
        kit::key_cap(&mut inner, colors, "/");
    }
    let field = TextEdit::singleline(&mut view.filter_text)
        .id(id)
        .hint_text(kit::mono("Filter names", 12.0).color(colors.fg(Token::FgDim)))
        .font(egui::FontId::new(12.0, egui::FontFamily::Monospace))
        .frame(egui::Frame::NONE)
        .align(egui::Align2::LEFT_CENTER)
        .min_size(vec2(0.0, 26.0))
        .desired_width(inner.available_width());
    let r = inner.add(field);
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
}

/// The type picker's button, which says which types are shown while the filter is on; and a
/// button that turns the filter off.
fn types(ui: &mut Ui, s: &SessionState, colors: &Colors, view: &ViewState, out: &mut Vec<Intent>) {
    if s.types_active() {
        if quiet(ui, colors, "All files", false, first_key(Action::Types)).clicked() {
            out.push(Intent::Type(dfdu_session::type_filter::Row::All));
        }
        let b = egui::Button::new(
            kit::label(format!("Only {}", s.types), 12.0).color(colors.fg(Token::StatusWarn)),
        )
        .frame(false)
        .selected(view.types.is_some())
        .min_size(vec2(0.0, 24.0));
        if ui.add(b).clicked() {
            out.push(Intent::Act(Action::Types));
        }
    } else if quiet(
        ui,
        colors,
        "File types…",
        view.types.is_some(),
        first_key(Action::Types),
    )
    .clicked()
    {
        out.push(Intent::Act(Action::Types));
    }
}

/// `00:16`.
pub fn mmss(d: Duration) -> String {
    let s = d.as_secs();
    format!("{:02}:{:02}", s / 60, s % 60)
}

/// How long a scan took: `6.8 s`, `2 min 05 s`.
pub fn took(d: Duration) -> String {
    let secs = d.as_secs();
    if secs < 60 {
        format!("{:.1} s", d.as_secs_f64())
    } else if secs < 3600 {
        format!("{} min {:02} s", secs / 60, secs % 60)
    } else {
        format!("{} h {:02} min", secs / 3600, secs / 60 % 60)
    }
}

/// The scan's state, drawn right to left so it reads left to right: a dot (or a spinner while
/// the scan runs), what the scan does, the totals, its errors (a click lists them), what makes
/// it partial, and whether the browser is read-only.
fn scan_state(ui: &mut Ui, s: &SessionState, colors: &Colors, now: Instant, out: &mut Vec<Intent>) {
    let elapsed = mmss(now.duration_since(s.started));
    let (word, rest, token) = match (s.cached, s.done_at) {
        (Some(Cached::Loading), _) => ("Loading", format!("the cached scan {elapsed}"), None),
        (Some(Cached::Refreshing { written }), _) => {
            let age = dfdu_session::cache_link::ago(
                written,
                dfdu_tree::now_secs().max(0).cast_unsigned(),
            );
            ("Cached", format!("{age} ago, refreshing {elapsed}"), None)
        }
        (None, Some(done)) if !s.snap.busy => (
            "Done",
            format!("in {}", took(done.duration_since(s.started))),
            Some(Token::ScanDone),
        ),
        _ => ("Scanning", elapsed.clone(), None),
    };
    let root = s.snap.root_total;
    let dim = colors.fg(Token::FgDim);
    let small = |t: String| kit::sans(t, 12.0).color(dim);
    ui.spacing_mut().item_spacing.x = 8.0;
    if s.read_only {
        kit::badge(ui, "Read-only", dim);
    }
    if s.snap.stalled > 0 {
        let text = format!(
            "{} director{} not responding",
            s.snap.stalled,
            if s.snap.stalled == 1 { "y" } else { "ies" }
        );
        ui.label(kit::sans(text, 12.0).color(colors.fg(Token::StatusWarn)));
    }
    if s.snap.incomplete {
        ui.label(kit::sans("incomplete", 12.0).color(colors.fg(Token::StatusWarn)));
    }
    if root.errors > 0 {
        let text = format!("{} unreadable", count(root.errors));
        let link = egui::Link::new(kit::sans(text, 12.0).color(colors.fg(Token::StatusWarn)));
        if ui.add(link).on_hover_text("List the errors (!)").clicked() {
            out.push(Intent::Act(Action::Errors));
        }
    }
    ui.spacing_mut().item_spacing.x = 0.0;
    let totals = format!(
        "{rest} · {} item{} · {}",
        count(root.items),
        plural(root.items),
        human(s.size_mode.of(&root))
    );
    if root.errors > 0 {
        ui.label(small(" ·".into()));
    }
    ui.label(small(totals));
    let color = token.map_or(colors.fg(Token::ScanActive), |t| colors.fg(t));
    ui.add_space(4.0);
    ui.label(kit::title(word, 12.0).color(color));
    ui.add_space(8.0);
    match token {
        Some(_) => kit::dot(ui, color),
        None => spinner(ui, color, now.duration_since(s.started)),
    }
    ui.spacing_mut().item_spacing.x = 12.0;
}

/// A spinner turned by `elapsed`. Unlike egui's, it asks for no frames: the session asks for
/// one each time a spinner is due to move.
fn spinner(ui: &mut Ui, color: egui::Color32, elapsed: Duration) {
    let size = 14.0;
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

/// Which breadcrumbs fit in `room` when each is `widths[i]` wide and a separator `sep`:
/// the first and the last ones always, the middle ones from the end while they fit, the
/// others one `…` (`None`).
pub fn fitting(widths: &[f32], sep: f32, ellipsis: f32, room: f32) -> Vec<Option<usize>> {
    let n = widths.len();
    let total: f32 = widths.iter().sum::<f32>() + sep * n.saturating_sub(1) as f32;
    if total <= room || n <= 2 {
        return (0..n).map(Some).collect();
    }
    let mut used = widths[0] + widths[n - 1] + 2.0 * sep + ellipsis + sep;
    let mut kept = Vec::new();
    for i in (1..n - 1).rev() {
        if used + widths[i] + sep > room {
            break;
        }
        used += widths[i] + sep;
        kept.push(i);
    }
    kept.reverse();
    let mut out = vec![Some(0)];
    if kept.first() != Some(&1) {
        out.push(None);
    }
    out.extend(kept.into_iter().map(Some));
    out.push(Some(n - 1));
    out
}

/// The breadcrumbs in mono: the directories above as dim links, the current one in `dir`.
/// When they do not fit, the middle ones fold into `…` and the current one is cut short.
fn breadcrumbs(ui: &mut Ui, s: &SessionState, colors: &Colors, out: &mut Vec<Intent>) {
    let snap = &s.snap;
    let segs = segments(&snap.dir_path, &snap.root_path, s.home.as_deref());
    let last = segs.len() - 1;
    let gap = 6.0;
    ui.spacing_mut().item_spacing.x = gap;
    let font = egui::FontId::new(13.0, egui::FontFamily::Monospace);
    let width = |t: &str| {
        ui.painter()
            .layout_no_wrap(t.to_owned(), font.clone(), egui::Color32::WHITE)
            .size()
            .x
    };
    let widths: Vec<f32> = segs.iter().map(|g| width(&g.text)).collect();
    let sep = width(SEPARATOR) + 2.0 * gap;
    let room = ui.available_width();
    let shown = fitting(&widths, sep, width("…"), room);
    for (k, at) in shown.iter().enumerate() {
        if k > 0 {
            ui.label(kit::mono(SEPARATOR, 13.0).color(colors.fg(Token::Border)));
        }
        let Some(i) = *at else {
            ui.label(kit::mono("…", 13.0).color(colors.fg(Token::FgDim)));
            continue;
        };
        let seg = &segs[i];
        if i == last {
            let label =
                egui::Label::new(kit::mono_bold(&seg.text, 13.0).color(colors.fg(Token::Dir)))
                    .truncate();
            ui.add(label).on_hover_text(seg.text.as_str());
            break;
        }
        let link = egui::Link::new(kit::mono(&seg.text, 13.0).color(colors.fg(Token::FgDim)));
        if ui.add(link).clicked() {
            out.push(Intent::GoTo {
                child: segs[i + 1].dir.clone(),
            });
        }
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

    #[test]
    fn breadcrumbs_that_do_not_fit_fold_the_middle_ones() {
        let w = [50.0, 30.0, 30.0, 30.0, 40.0];
        assert_eq!(fitting(&w, 10.0, 10.0, 1000.0).len(), 5);
        // Room for the ends, the ellipsis and one more.
        assert_eq!(
            fitting(&w, 10.0, 10.0, 50.0 + 40.0 + 30.0 + 10.0 + 4.0 * 10.0),
            [Some(0), None, Some(3), Some(4)]
        );
        assert_eq!(fitting(&w, 10.0, 10.0, 0.0), [Some(0), None, Some(4)]);
    }
}
