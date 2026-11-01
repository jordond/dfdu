//! What the review views share: the keys that act on their own rows ([`intent`]), as the
//! terminal's do, and the tick drawn on a row ([`tick`]).

use dfdu_session::SessionState;
use dfdu_session::actions::{Action, cursor_after};
use dfdu_session::cleanup;
use dfdu_session::palette::Token;
use egui::{Rect, Sense, Stroke, Ui, vec2};

use crate::browser::Intent;
use crate::theme::Colors;

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
/// move its cursor, `Enter` and `→` open a row, `←` closes the row's section and `Space` ticks.
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
    None
}

/// A row's tick: a box, filled when on, with a bar when partly on; a locked one is a crossed
/// box that takes no click. Named for what a click does.
pub fn tick(ui: &mut Ui, colors: &Colors, tick: Tick) -> egui::Response {
    let h = ui.text_style_height(&egui::TextStyle::Body);
    let sense = if tick == Tick::Locked {
        Sense::hover()
    } else {
        Sense::click()
    };
    let (rect, r) = ui.allocate_exact_size(vec2(h, h), sense);
    let side = h * 0.62;
    let b = Rect::from_center_size(rect.center(), vec2(side, side));
    let token = match tick {
        Tick::On | Tick::Partial => Token::Mark,
        Tick::Off if r.hovered() => Token::Fg,
        Tick::Off | Tick::Locked => Token::FgDim,
    };
    let color = colors.fg(token);
    let p = ui.painter();
    p.rect_stroke(b, 2.0, Stroke::new(1.2, color), egui::StrokeKind::Inside);
    match tick {
        Tick::On => {
            p.rect_filled(b.shrink(side * 0.22), 1.0, color);
        }
        Tick::Partial => {
            let y = b.center().y;
            let x = (b.left() + side * 0.25)..=(b.right() - side * 0.25);
            p.hline(x, y, Stroke::new(2.0, color));
        }
        Tick::Locked => {
            p.line_segment([b.left_top(), b.right_bottom()], Stroke::new(1.2, color));
        }
        Tick::Off => {}
    }
    let (role, what, on) = match tick {
        Tick::On => (egui::Role::CheckBox, "Untick", true),
        Tick::Off | Tick::Partial => (egui::Role::CheckBox, "Tick", false),
        Tick::Locked => (egui::Role::Label, "Protected", false),
    };
    r.widget_info(|| egui::WidgetInfo::selected(role, true, on, what));
    r
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
