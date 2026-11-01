//! What the review views (Cleanup, Duplicates and Largest) share: the keys that act on their own
//! rows ([`intent`]), as the terminal's do, the tick drawn on a row ([`tick`]), the summary bar
//! at the top of the sheet ([`summary`]) and what a view without rows says ([`empty`]).

use dfdu_session::SessionState;
use dfdu_session::actions::{Action, cursor_after};
use dfdu_session::palette::Token;
use dfdu_session::{cleanup, dupes};
use egui::{
    Align, CornerRadius, Frame, Layout, Margin, Rect, RichText, Sense, Shape, Stroke, StrokeKind,
    Ui, pos2, vec2,
};

use crate::browser::Intent;
use crate::kit;
use crate::theme::{Colors, style};

/// The tick of a row of a review view.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Tick {
    On,
    Off,
    /// A section or a category with some of its items ticked.
    Partial,
    /// Cannot be ticked (protected).
    Locked,
}

/// The tick of `of` items with `ticked` of them ticked.
pub fn tick_of(ticked: usize, of: usize) -> Tick {
    match ticked {
        0 => Tick::Off,
        n if n >= of => Tick::On,
        _ => Tick::Partial,
    }
}

/// What action `a` asks of the review view shown, if it acts on the view's own rows: the moves
/// move its cursor, `Enter` and `→` open a row (before the first comparison of duplicates,
/// `Enter` starts it), `←` closes the row's section or group and `Space` ticks.
pub fn intent(s: &SessionState, a: Action) -> Option<Intent> {
    let page = s.page_rows();
    if cleanup::active(s) {
        use cleanup::Intent as C;
        let last = cleanup::rows(s).len().saturating_sub(1);
        let i = match cursor_after(a, s.cleanup.cursor, last, page) {
            Some(c) => C::Select(c),
            None => match a {
                Action::Open | Action::Enter => C::Activate,
                Action::Parent => C::CloseUp,
                Action::ToggleMark => C::Toggle,
                _ => return None,
            },
        };
        return Some(Intent::Cleanup(i));
    }
    if dupes::active(s) {
        use dupes::Intent as D;
        if s.dupes.run.is_none() && a == Action::Enter {
            return Some(Intent::Dupes(D::Compare));
        }
        let last = dupes::rows(s).len().saturating_sub(1);
        let i = match cursor_after(a, s.dupes.cursor, last, page) {
            Some(c) => D::Select(c),
            None => match a {
                Action::Open | Action::Enter => D::Activate,
                Action::Parent => D::CloseUp,
                Action::ToggleMark => D::Toggle,
                _ => return None,
            },
        };
        return Some(Intent::Dupes(i));
    }
    None
}

/// The height of a row of a review view's table.
pub const ROW: f32 = 26.0;

/// The opacity of what is disabled.
const DISABLED: f32 = 0.45;

/// A row's tick, drawn as the list's mark is: a box filled in `mark` with a tick when on, with a
/// bar when partly on, outlined when off; a locked one is a disabled box that takes no click.
/// Named for what a click does.
pub fn tick(ui: &mut Ui, colors: &Colors, tick: Tick) -> egui::Response {
    if tick == Tick::Locked {
        return locked(ui, colors, "Protected");
    }
    let (rect, r) = ui.allocate_exact_size(vec2(16.0, 16.0), Sense::click());
    let b = Rect::from_center_size(rect.center(), vec2(14.0, 14.0));
    let radius = CornerRadius::same(style::RADIUS_SM);
    let p = ui.painter();
    let on = colors.chrome.surface;
    match tick {
        Tick::On => {
            p.rect_filled(b, radius, colors.fg(Token::Mark));
            let c = b.center();
            let tick = vec![
                pos2(c.x - 3.5, c.y),
                pos2(c.x - 1.0, c.y + 2.6),
                pos2(c.x + 3.6, c.y - 2.8),
            ];
            p.add(Shape::line(tick, Stroke::new(1.8, on)));
        }
        Tick::Partial => {
            p.rect_filled(b, radius, colors.fg(Token::Mark));
            let x = (b.left() + 3.5)..=(b.right() - 3.5);
            p.hline(x, b.center().y, Stroke::new(1.8, on));
        }
        Tick::Off | Tick::Locked => {
            let edge = if r.hovered() {
                colors.fg(Token::BorderFocus)
            } else {
                colors.fg(Token::Border)
            };
            p.rect_stroke(b, radius, Stroke::new(1.0, edge), StrokeKind::Inside);
        }
    }
    let (what, on) = match tick {
        Tick::On => ("Untick", true),
        Tick::Off | Tick::Partial | Tick::Locked => ("Tick", false),
    };
    r.widget_info(|| egui::WidgetInfo::selected(egui::Role::CheckBox, true, on, what));
    r
}

/// A disabled tick, on a row that cannot be ticked: a faded box that takes no click, named
/// `name`.
pub fn locked(ui: &mut Ui, colors: &Colors, name: &str) -> egui::Response {
    let (rect, r) = ui.allocate_exact_size(vec2(16.0, 16.0), Sense::hover());
    let b = Rect::from_center_size(rect.center(), vec2(14.0, 14.0));
    let radius = CornerRadius::same(style::RADIUS_SM);
    let p = ui.painter();
    p.rect_filled(b, radius, colors.chrome.hover.gamma_multiply(DISABLED));
    let edge = colors.fg(Token::Border).gamma_multiply(DISABLED);
    p.rect_stroke(b, radius, Stroke::new(1.0, edge), StrokeKind::Inside);
    r.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Label, true, name));
    r
}

/// The summary bar at the top of a review view's sheet: `big` in mono and `word` after it in
/// the title style, `line` dim beside them, then the controls `right` adds from the right end;
/// a rule in `border` under it.
pub fn summary(
    ui: &mut Ui,
    colors: &Colors,
    (big, word): (&str, &str),
    line: &str,
    right: impl FnOnce(&mut Ui),
) {
    let header = colors.fg(Token::Header);
    Frame::new()
        .inner_margin(Margin::symmetric(16, 12))
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = style::SPACE_2;
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 6.0;
                    ui.label(kit::mono_bold(big, 20.0).color(header));
                    ui.label(kit::title(word, 20.0).color(header));
                });
                ui.add_space(style::SPACE_1);
                ui.label(kit::sans(line, 12.0).color(colors.fg(Token::FgDim)));
                ui.with_layout(Layout::right_to_left(Align::Center), right);
            });
        });
    let x = ui.max_rect().x_range();
    let y = ui.cursor().top();
    ui.painter()
        .hline(x, y, Stroke::new(1.0, colors.fg(Token::Border)));
    ui.add_space(style::SPACE_1);
}

/// A line under the summary bar, dim, indented as the bar is.
pub fn note(ui: &mut Ui, colors: &Colors, text: &str) {
    Frame::new()
        .inner_margin(Margin::symmetric(16, 4))
        .show(ui, |ui| {
            ui.label(kit::sans(text, 12.0).color(colors.fg(Token::FgDim)));
        });
}

/// What a review view says when it has no rows: `lines` centred in it, the first in `fg`, the
/// rest dim, then what `then` adds (its button).
pub fn empty(ui: &mut Ui, colors: &Colors, lines: &[&str], then: impl FnOnce(&mut Ui)) {
    #[expect(clippy::cast_precision_loss, reason = "a few lines")]
    let height = 22.0 * lines.len() as f32 + 48.0;
    ui.add_space(((ui.available_height() - height) / 2.0).max(style::SPACE_6));
    ui.vertical_centered(|ui| {
        ui.set_max_width(520.0);
        for (i, l) in lines.iter().enumerate() {
            let token = if i == 0 { Token::Fg } else { Token::FgDim };
            ui.add(egui::Label::new(kit::sans(*l, 13.0).color(colors.fg(token))).wrap());
        }
        ui.add_space(style::SPACE_3);
        then(ui);
    });
}

/// A hairline in `color` under the row whose cell `ui` is, across the table.
pub fn rule_under(ui: &Ui, color: egui::Color32) {
    let y = ui.max_rect().bottom() - 0.5;
    let x = ui.max_rect().left()..=ui.clip_rect().right();
    ui.painter().hline(x, y, Stroke::new(1.0, color));
}

/// `text` aligned to the right of its cell.
pub fn right(ui: &mut Ui, text: RichText) {
    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
        ui.label(text);
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_section_ticks_partly() {
        assert_eq!(tick_of(0, 3), Tick::Off);
        assert_eq!(tick_of(2, 3), Tick::Partial);
        assert_eq!(tick_of(3, 3), Tick::On);
    }
}
