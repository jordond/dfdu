//! The lines below the list: the prompt about directories that do not respond, the status line
//! and the key hints.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use super::spread;
use crate::app::keys::{Action, first_key};
use crate::app::{App, Overlay};
use crate::features::details;
use crate::model::snapshot::Mark;
use crate::state::{filter, marks};

/// Drawn only while directories are stalled (the row has no height otherwise).
pub fn stalled(f: &mut Frame, app: &App, area: Rect) {
    let n = app.snap.stalled;
    let text = format!(
        " {n} director{} not responding: wait, or press x to skip {}",
        if n == 1 { "y is" } else { "ies are" },
        if n == 1 { "it" } else { "them" }
    );
    f.render_widget(
        Paragraph::new(Line::styled(text, app.theme.status_warn)),
        area,
    );
}

/// Delete progress, else the last status message or engine notice; what is marked on the right.
/// Without any of those, the list view shows the selection line there, unless an overlay is
/// open over it.
pub fn status(f: &mut Frame, app: &App, area: Rect, list_view: bool) {
    let msg = match app.snap.deleting {
        Some((done, total)) => format!("deleting {done}/{total}…"),
        None if app.snap.undoing => "putting back…".into(),
        None => app
            .status
            .clone()
            .or_else(|| app.snap.notice.clone())
            .unwrap_or_default(),
    };
    if msg.is_empty()
        && list_view
        && matches!(app.overlay, Overlay::None)
        && let Some(line) = super::selection::line(app, area.width)
    {
        f.render_widget(Paragraph::new(line), area);
        return;
    }
    let line = match marks::summary(app) {
        Some(marked) => spread(
            vec![Span::raw(format!(" {msg}"))],
            vec![marked, Span::raw(" ")],
            area.width,
        )
        .style(app.theme.fg_dim),
        None => Line::styled(format!(" {msg}"), app.theme.fg_dim),
    };
    f.render_widget(Paragraph::new(line), area);
}

/// At most this many hints on the line.
const MAX_HINTS: usize = 10;

/// One key hint: the keys and a short label. When the line is too short, the hints with the
/// highest `drop` go first.
#[derive(Clone, Debug, PartialEq)]
struct Hint {
    keys: String,
    label: &'static str,
    drop: u8,
}

fn hint(keys: impl Into<String>, label: &'static str, drop: u8) -> Hint {
    Hint {
        keys: keys.into(),
        label,
        drop,
    }
}

/// The hints for what the keys do now, in the order they are shown; in a read-only browser
/// without the keys that delete.
fn context(app: &App) -> Vec<Hint> {
    let mut hints = offered_now(app);
    if app.read_only {
        let deleting = [Action::Trash, Action::DeletePermanent, Action::Undo].map(first_key);
        hints.retain(|h| !deleting.contains(&h.keys.as_str()));
    }
    hints
}

/// The hints for what the keys do now. Whatever takes the keys first decides, as in
/// [`App::key`]: the open overlay, the filter input, the focused details pane, or else the list
/// (with what marks, a running scan and errors add).
fn offered_now(app: &App) -> Vec<Hint> {
    let arrows = format!("{}{}", first_key(Action::Up), first_key(Action::Down));
    let help = hint(first_key(Action::Help), "help", 0);
    let quit = hint(first_key(Action::Quit), "quit", 1);
    if let Some(hints) = overlay_hints(&app.overlay, &arrows) {
        return hints;
    }
    if filter::editing(app) {
        return editing(&arrows, "move");
    }
    if details::focused(app) {
        return vec![
            hint(first_key(Action::FocusDetails), "back", 2),
            hint(arrows, "scroll", 2),
            hint(first_key(Action::Details), "close", 3),
            help,
            quit,
        ];
    }
    if crate::features::largest::active(app) {
        return vec![
            hint(arrows, "move", 3),
            hint("Enter", "go to", 2),
            hint(first_key(Action::ToggleMark), "mark", 1),
            hint(first_key(Action::CopyPath), "copy path", 5),
            hint(first_key(Action::OpenPath), "open", 6),
            hint(first_key(Action::Trash), "trash marked", 0),
            hint(first_key(Action::ViewList), "list", 3),
            help,
            quit,
        ];
    }
    if crate::features::dupes::active(app) {
        return vec![
            hint(arrows.clone(), "move", 3),
            hint(first_key(Action::ToggleMark), "mark", 1),
            hint("a", "all copies", 2),
            hint(first_key(Action::DupesKeep), "keep rule", 6),
            hint("Enter", "go to", 4),
            hint("r", "compare again", 5),
            hint(first_key(Action::Trash), "trash marked", 0),
            hint(first_key(Action::ViewList), "list", 3),
            help.clone(),
            quit.clone(),
        ];
    }
    if crate::features::cleanup::active(app) {
        return vec![
            hint(arrows, "move", 3),
            hint("Enter", "open", 2),
            hint(first_key(Action::ToggleMark), "mark", 1),
            hint("a", "recommended", 4),
            hint("f", "filter", 5),
            hint("n", "command", 4),
            hint(first_key(Action::Trash), "trash marked", 0),
            hint(first_key(Action::ViewList), "list", 3),
            help,
            quit,
        ];
    }
    if crate::features::treemap::active(app) {
        let mut hints = vec![
            hint("←↑↓→", "move", 2),
            hint("Enter", "zoom in", 3),
            hint("Backspace", "zoom out", 4),
            hint("+ -", "depth", 6),
            hint(first_key(Action::TreemapColor), "colour", 7),
            hint(first_key(Action::ToggleMark), "mark", 6),
            hint(first_key(Action::Trash), "trash", 5),
            hint(first_key(Action::ViewList), "list", 3),
        ];
        hints.push(help);
        hints.push(quit);
        return hints;
    }
    list_hints(app, arrows, help, quit)
}

/// The hints of a filter or search being typed, where the arrows do `arrows_do`.
fn editing(arrows: &str, arrows_do: &'static str) -> Vec<Hint> {
    vec![
        hint("Enter", "keep", 0),
        hint("Esc", "clear", 0),
        hint(arrows, arrows_do, 1),
    ]
}

/// The hints of the open overlay; `None` when none is open.
fn overlay_hints(overlay: &Overlay, arrows: &str) -> Option<Vec<Hint>> {
    Some(match overlay {
        Overlay::None => return None,
        Overlay::ConfirmDelete(_) => {
            vec![
                hint("p", "permanent", 0),
                hint("s", "stage", 1),
                hint("Esc", "cancel", 0),
            ]
        }
        Overlay::Help(h) if h.searching() => editing(arrows, "scroll"),
        Overlay::Help(_) => {
            vec![
                hint(arrows, "scroll", 1),
                hint("PgUp PgDn", "page", 2),
                hint("/", "search", 1),
                hint("Esc", "close", 0),
            ]
        }
        Overlay::Palette(_) => {
            vec![
                hint(arrows, "move", 1),
                hint("Enter", "run", 0),
                hint("Esc", "close", 0),
            ]
        }
        Overlay::Errors { .. } => {
            vec![
                hint(arrows, "move", 1),
                hint("Enter", "go to it", 0),
                hint("Esc", "close", 0),
            ]
        }
        Overlay::Types(_) => {
            vec![
                hint(arrows, "move", 1),
                hint("Space", "toggle", 0),
                hint("Enter", "done", 0),
                hint("Esc", "close", 0),
            ]
        }
        Overlay::MarkPattern(_) => {
            vec![hint("Enter", "mark", 0), hint("Esc", "cancel", 0)]
        }
    })
}

/// The hints of the list, with what marks, a running scan and errors add.
fn list_hints(app: &App, arrows: String, help: Hint, quit: Hint) -> Vec<Hint> {
    let sort = [
        Action::SortSize,
        Action::SortName,
        Action::SortItems,
        Action::SortMtime,
    ]
    .map(first_key)
    .join(" ");
    let mut hints = vec![
        hint(arrows, "move", 2),
        hint(first_key(Action::Open), "open", 3),
    ];
    hints.push(hint(first_key(Action::Parent), "up", 4));
    if app
        .selected_entry()
        .is_some_and(|e| matches!(e.mark, Mark::OtherFs | Mark::Network))
    {
        hints.push(hint(first_key(Action::EnterMount), "scan it", 1));
    }
    if !app.marks.is_empty() {
        hints.push(hint(first_key(Action::ToggleMark), "mark", 2));
        hints.push(hint(first_key(Action::Trash), "trash marked", 2));
        hints.push(hint(first_key(Action::DeletePermanent), "delete marked", 3));
    }
    hints.push(hint(sort, "sort", 7));
    if app.snap.busy {
        hints.push(hint(first_key(Action::Resort), "re-sort", 8));
    }
    if app.marks.is_empty() {
        hints.push(hint(first_key(Action::ToggleMark), "mark", 6));
    }
    hints.push(hint(first_key(Action::Filter), "filter", 5));
    hints.push(hint(first_key(Action::Details), "details", 9));
    hints.push(hint(first_key(Action::Palette), "commands", 9));
    if app.snap.root_total.errors > 0 {
        hints.push(hint(first_key(Action::Errors), "errors", 4));
    }
    hints.push(help);
    hints.push(quit);
    hints
}

/// The hints of `hints` that fit on `width` cells after the leading space, at most
/// [`MAX_HINTS`], whole and in their order; the ones to drop first make room for the others.
fn fit(hints: &[Hint], width: usize) -> Vec<&Hint> {
    let mut order: Vec<usize> = (0..hints.len()).collect();
    order.sort_by_key(|&i| hints[i].drop);
    let mut keep = vec![false; hints.len()];
    let (mut used, mut kept) = (1, 0);
    for i in order {
        let h = &hints[i];
        let w = Span::raw(&h.keys).width() + 1 + Span::raw(h.label).width();
        let gap = if kept == 0 { 0 } else { 2 };
        if kept < MAX_HINTS && used + gap + w <= width {
            keep[i] = true;
            used += gap + w;
            kept += 1;
        }
    }
    hints
        .iter()
        .zip(keep)
        .filter(|(_, k)| *k)
        .map(|(h, _)| h)
        .collect()
}

/// The hints line for `width` cells.
fn line(app: &App, width: u16) -> Line<'static> {
    let t = &app.theme;
    let hints = context(app);
    let mut spans = vec![Span::raw(" ")];
    for (i, h) in fit(&hints, usize::from(width)).into_iter().enumerate() {
        if i > 0 {
            spans.push(Span::raw("  "));
        }
        spans.push(Span::styled(h.keys.clone(), t.key));
        spans.push(Span::styled(format!(" {}", h.label), t.fg_dim));
    }
    Line::from(spans)
}

/// The most used keys for the current context, as many as fit.
pub fn hints(f: &mut Frame, app: &App, area: Rect) {
    f.render_widget(Paragraph::new(line(app, area.width)), area);
}

#[cfg(test)]
mod tests {
    use std::time::Instant;

    use ratatui::crossterm::event::KeyCode;

    use super::*;
    use crate::tests::{library_snap, press, render};
    use crate::theme::Theme;

    fn app(busy: bool, errors: u64, t0: Instant) -> App {
        let mut app = App::new(Theme::no_color(), t0);
        let mut snap = library_snap();
        snap.busy = busy;
        snap.root_total.errors = errors;
        app.refresh(snap, t0);
        app
    }

    fn shown(app: &App, width: u16) -> String {
        line(app, width)
            .spans
            .iter()
            .map(|s| s.content.as_ref())
            .collect()
    }

    #[test]
    fn hints_change_with_context() {
        let t0 = Instant::now();
        let mut list = app(false, 0, t0);
        assert_eq!(
            shown(&list, 120),
            " ↑↓ move  → open  ← up  s n C M sort  Space mark  / filter  i details  : commands  \
             ? help  q quit"
        );
        let scanning = app(true, 0, t0);
        assert!(shown(&scanning, 200).contains("  S re-sort  "));
        assert!(!shown(&list, 200).contains("S re-sort"));
        let errors = app(false, 3, t0);
        assert!(shown(&errors, 200).contains("  ! errors  ? help"));
        assert!(!shown(&list, 200).contains("! errors"));
        // Never more than ten, whatever the width.
        let busy_errors = app(true, 3, t0);
        let all = shown(&busy_errors, 400);
        assert_eq!(all.matches("  ").count() + 1, MAX_HINTS, "{all}");
        assert!(
            all.contains("S re-sort") && all.contains("! errors"),
            "{all}"
        );

        list.key(press(KeyCode::Char(' ')), t0);
        assert_eq!(
            shown(&list, 120),
            " ↑↓ move  → open  ← up  Space mark  d trash marked  D delete marked  s n C M sort  \
             / filter  ? help  q quit"
        );
        list.key(press(KeyCode::Char('D')), t0);
        assert!(matches!(list.overlay, Overlay::ConfirmDelete(_)));
        assert_eq!(shown(&list, 120), " p permanent  s stage  Esc cancel");
        list.key(press(KeyCode::Esc), t0);

        list.key(press(KeyCode::Char('/')), t0);
        assert_eq!(shown(&list, 120), " Enter keep  Esc clear  ↑↓ move");
        list.key(press(KeyCode::Esc), t0);

        list.key(press(KeyCode::Char('i')), t0);
        render(&mut list, t0, 120, 20);
        list.key(press(KeyCode::Tab), t0);
        assert!(details::focused(&list));
        assert_eq!(
            shown(&list, 120),
            " Tab back  ↑↓ scroll  i close  ? help  q quit"
        );
        // The footer of a frame is the line for its width.
        let frame = render(&mut list, t0, 120, 20);
        assert_eq!(frame.lines().last(), Some(shown(&list, 120).trim_end()));
        // An overlay takes the keys before the pane, and so do the hints.
        list.key(press(KeyCode::Char('d')), t0);
        assert!(matches!(list.overlay, Overlay::ConfirmDelete(_)));
        assert!(details::focused(&list));
        assert_eq!(shown(&list, 120), " p permanent  s stage  Esc cancel");
        list.key(press(KeyCode::Esc), t0);

        for (open, want) in overlays() {
            let mut app = app(true, 1, t0);
            open(&mut app);
            assert!(!matches!(app.overlay, Overlay::None));
            assert_eq!(shown(&app, 120), want);
        }
    }

    /// Opens an overlay.
    type Open = fn(&mut App);

    /// Ways to open each overlay but the delete dialog, and the hints while it is open.
    fn overlays() -> [(Open, &'static str); 4] {
        [
            (
                |app| {
                    app.key(press(KeyCode::Char('?')), Instant::now());
                },
                " ↑↓ scroll  PgUp PgDn page  / search  Esc close",
            ),
            (
                |app| {
                    app.key(press(KeyCode::Char('?')), Instant::now());
                    app.key(press(KeyCode::Char('/')), Instant::now());
                },
                " Enter keep  Esc clear  ↑↓ scroll",
            ),
            (
                |app| {
                    app.key(press(KeyCode::Char(':')), Instant::now());
                },
                " ↑↓ move  Enter run  Esc close",
            ),
            (
                |app| {
                    app.show_errors(dfdu_engine::ErrorList {
                        root: "/r".into(),
                        items: Vec::new(),
                        total: 0,
                        kinds: Vec::new(),
                    });
                },
                " ↑↓ move  Enter go to it  Esc close",
            ),
        ]
    }

    #[test]
    fn hints_never_cut_a_key() {
        let t0 = Instant::now();
        let mut marked = app(true, 2, t0);
        marked.key(press(KeyCode::Char(' ')), t0);
        let mut filtering = app(false, 0, t0);
        filtering.key(press(KeyCode::Char('/')), t0);
        let mut focused = app(false, 0, t0);
        focused.key(press(KeyCode::Char('i')), t0);
        render(&mut focused, t0, 120, 20);
        focused.key(press(KeyCode::Tab), t0);
        assert!(details::focused(&focused));
        let mut dialog = app(false, 0, t0);
        dialog.key(press(KeyCode::Char('d')), t0);
        assert!(matches!(dialog.overlay, Overlay::ConfirmDelete(_)));
        let mut apps = vec![
            app(false, 0, t0),
            app(true, 1, t0),
            marked,
            filtering,
            focused,
            dialog,
        ];
        for (open, _) in overlays() {
            let mut app = app(true, 1, t0);
            open(&mut app);
            apps.push(app);
        }
        for app in apps {
            let whole: Vec<String> = context(&app)
                .iter()
                .map(|h| format!("{} {}", h.keys, h.label))
                .collect();
            for width in 40..=120 {
                let text = shown(&app, width);
                assert!(
                    Span::raw(&text).width() <= usize::from(width),
                    "{width}: {text}"
                );
                for part in text[1..].split("  ") {
                    assert!(
                        whole.iter().any(|w| w == part),
                        "{width}: {part:?} in {text}"
                    );
                }
                // The way to the rest stays on the line.
                assert!(text.contains("? help") || text.contains("Esc "), "{text}");
            }
        }
    }
}
