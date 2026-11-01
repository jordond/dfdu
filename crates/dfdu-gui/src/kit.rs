//! The design system's parts, drawn with egui: the text styles of data and labels, buttons
//! (secondary, primary in the accent, danger in `status.error`), segmented controls, key caps,
//! category chips, safety badges, size bars, status dots, hairlines and sheets.

#![allow(dead_code, reason = "parts the review views and dialogs are moving to")]

use egui::{
    Align2, Button, Color32, CornerRadius, FontFamily, FontId, Frame, Margin, Rect, Response,
    RichText, Sense, Stroke, StrokeKind, Ui, WidgetInfo, pos2, vec2,
};

use dfdu_session::palette::Token;

use crate::theme::Colors;
use crate::theme::style::{
    CONTROL, MONO_SEMIBOLD_FAMILY, RADIUS_MD, RADIUS_SM, SANS_MEDIUM_FAMILY, SANS_SEMIBOLD_FAMILY,
};

/// Mono text of `size`: data, sizes, paths, names.
pub fn mono(text: impl Into<String>, size: f32) -> RichText {
    RichText::new(text).font(FontId::new(size, FontFamily::Monospace))
}

/// Semibold mono text of `size`: sizes that stand out, key caps, the big total.
pub fn mono_bold(text: impl Into<String>, size: f32) -> RichText {
    RichText::new(text).font(FontId::new(
        size,
        FontFamily::Name(MONO_SEMIBOLD_FAMILY.into()),
    ))
}

/// Sans text of `size`: copy.
pub fn sans(text: impl Into<String>, size: f32) -> RichText {
    RichText::new(text).font(FontId::new(size, FontFamily::Proportional))
}

/// Medium sans text of `size`: labels, buttons, tabs, column headers.
pub fn label(text: impl Into<String>, size: f32) -> RichText {
    RichText::new(text).font(FontId::new(
        size,
        FontFamily::Name(SANS_MEDIUM_FAMILY.into()),
    ))
}

/// Semibold sans text of `size`: titles.
pub fn title(text: impl Into<String>, size: f32) -> RichText {
    RichText::new(text).font(FontId::new(
        size,
        FontFamily::Name(SANS_SEMIBOLD_FAMILY.into()),
    ))
}

/// How a button looks.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// Outlined, on a raised fill.
    Secondary,
    /// The view's one main action, in the accent.
    Primary,
    /// Deleting: `status.error` fill.
    Danger,
}

/// A button `text` of `kind`, 28 high, with the key `key` in a cap on its right.
pub fn button(
    ui: &mut Ui,
    colors: &Colors,
    kind: Kind,
    text: &str,
    key: Option<&str>,
    enabled: bool,
) -> Response {
    let c = &colors.chrome;
    let (fill, fg, stroke) = match kind {
        Kind::Secondary => (c.raised, colors.fg(Token::Fg), colors.fg(Token::Border)),
        Kind::Primary => (colors.accent(), c.on_accent, colors.accent()),
        Kind::Danger => (
            colors.fg(Token::StatusError),
            c.on_danger,
            colors.fg(Token::StatusError),
        ),
    };
    let mut b = Button::new(label(text, 12.0).color(fg))
        .fill(fill)
        .stroke(Stroke::new(1.0, stroke))
        .corner_radius(CornerRadius::same(RADIUS_MD))
        .min_size(vec2(0.0, CONTROL));
    if let Some(key) = key {
        let cap = match kind {
            Kind::Secondary => colors.fg(Token::Key),
            Kind::Primary | Kind::Danger => fg,
        };
        b = b.right_text(mono_bold(key, 11.0).color(cap));
    }
    let r = ui.add_enabled(enabled, b);
    if key.is_some() {
        // Named by its text alone: the key is a hint.
        r.widget_info(|| WidgetInfo::labeled(egui::Role::Button, enabled, text));
    }
    r
}

/// A segmented control of `options` with `selected` filled in the accent, 24 high; returns
/// the option clicked when it is not the selected one. `name` is the group's accessible name.
pub fn segmented(
    ui: &mut Ui,
    colors: &Colors,
    name: &str,
    options: &[&str],
    selected: usize,
) -> Option<usize> {
    let c = &colors.chrome;
    let border = colors.fg(Token::Border);
    let font = FontId::new(12.0, FontFamily::Name(SANS_MEDIUM_FAMILY.into()));
    let pad = 8.0;
    let widths: Vec<f32> = options
        .iter()
        .map(|o| {
            let g = ui
                .painter()
                .layout_no_wrap((*o).to_owned(), font.clone(), Color32::WHITE);
            g.size().x + pad * 2.0
        })
        .collect();
    let total: f32 = widths.iter().sum();
    let (rect, group) = ui.allocate_exact_size(vec2(total, 24.0), Sense::hover());
    group.widget_info(|| WidgetInfo::labeled(egui::Role::Group, true, name));
    let mut clicked = None;
    let mut x = rect.left();
    for (i, (o, w)) in options.iter().zip(&widths).enumerate() {
        let seg = Rect::from_min_size(pos2(x, rect.top()), vec2(*w, rect.height()));
        let id = egui::Id::unique(("dfdu-segment", name, i));
        let r = ui.interact(seg, id, Sense::click());
        let on = i == selected;
        r.widget_info(|| WidgetInfo::selected(egui::Role::RadioButton, true, on, *o));
        let radius = match i {
            0 if options.len() == 1 => CornerRadius::same(RADIUS_MD),
            0 => CornerRadius {
                nw: RADIUS_MD,
                sw: RADIUS_MD,
                ne: 0,
                se: 0,
            },
            i if i + 1 == options.len() => CornerRadius {
                ne: RADIUS_MD,
                se: RADIUS_MD,
                nw: 0,
                sw: 0,
            },
            _ => CornerRadius::ZERO,
        };
        let p = ui.painter();
        if on {
            p.rect_filled(seg, radius, colors.accent());
        } else if r.hovered() {
            p.rect_filled(seg, radius, c.hover);
        }
        if i > 0 {
            p.vline(seg.left(), seg.y_range(), Stroke::new(1.0, border));
        }
        let fg = if on {
            c.on_accent
        } else {
            colors.fg(Token::Fg)
        };
        p.text(seg.center(), Align2::CENTER_CENTER, *o, font.clone(), fg);
        if r.clicked() && !on {
            clicked = Some(i);
        }
        x += w;
    }
    ui.painter().rect_stroke(
        rect,
        CornerRadius::same(RADIUS_MD),
        Stroke::new(1.0, border),
        StrokeKind::Inside,
    );
    clicked
}

/// A key cap: the key's name in mono, outlined, with a heavier bottom edge.
pub fn key_cap(ui: &mut Ui, colors: &Colors, key: &str) -> Response {
    let font = FontId::new(11.0, FontFamily::Name(MONO_SEMIBOLD_FAMILY.into()));
    let g = ui
        .painter()
        .layout_no_wrap(key.to_owned(), font, colors.fg(Token::Key));
    let size = vec2(g.size().x + 10.0, 18.0);
    let (rect, r) = ui.allocate_exact_size(size, Sense::hover());
    r.widget_info(|| WidgetInfo::labeled(egui::Role::Label, true, key));
    let p = ui.painter();
    let radius = CornerRadius::same(RADIUS_SM);
    let border = colors.fg(Token::Border);
    p.rect_filled(rect, radius, colors.chrome.hover);
    p.rect_stroke(rect, radius, Stroke::new(1.0, border), StrokeKind::Inside);
    p.hline(
        rect.x_range().shrink(1.5),
        rect.bottom() - 1.0,
        Stroke::new(1.0, border),
    );
    let at = rect.center() - g.size() / 2.0 - vec2(0.0, 0.5);
    p.galley(at, g, Color32::PLACEHOLDER);
    r
}

/// A key cap and what the key does after it: `[d] move`.
pub fn hint(ui: &mut Ui, colors: &Colors, key: &str, what: &str) {
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 4.0;
        key_cap(ui, colors, key);
        ui.label(sans(what, 12.0).color(colors.fg(Token::FgDim)));
    });
}

/// A category: an 8px square in its colour, then its name in it.
pub fn chip(ui: &mut Ui, name: &str, color: Color32) -> Response {
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 6.0;
        square(ui, color, 8.0);
        ui.label(label(name, 12.0).color(color));
    })
    .response
}

/// A filled square of `side`, with rounded corners: a category's swatch.
pub fn square(ui: &mut Ui, color: Color32, side: f32) {
    let (rect, _) = ui.allocate_exact_size(vec2(side, side), Sense::hover());
    ui.painter().rect_filled(rect, CornerRadius::same(2), color);
}

/// A status dot of 8px: the scan's state.
pub fn dot(ui: &mut Ui, color: Color32) {
    let (rect, _) = ui.allocate_exact_size(vec2(8.0, 8.0), Sense::hover());
    ui.painter().circle_filled(rect.center(), 4.0, color);
}

/// A badge: `text` in `color`, outlined in it.
pub fn badge(ui: &mut Ui, text: &str, color: Color32) -> Response {
    let font = FontId::new(12.0, FontFamily::Name(SANS_MEDIUM_FAMILY.into()));
    let g = ui.painter().layout_no_wrap(text.to_owned(), font, color);
    let (rect, r) = ui.allocate_exact_size(g.size() + vec2(16.0, 4.0), Sense::hover());
    r.widget_info(|| WidgetInfo::labeled(egui::Role::Label, true, text));
    let p = ui.painter();
    p.rect_stroke(
        rect,
        CornerRadius::same(RADIUS_SM),
        Stroke::new(1.0, color),
        StrokeKind::Inside,
    );
    p.galley(rect.min + vec2(8.0, 2.0), g, Color32::PLACEHOLDER);
    r
}

/// A size bar `width` wide and `height` thick, filled to `frac` in `bar.fill`.
pub fn size_bar(ui: &mut Ui, colors: &Colors, width: f32, height: f32, frac: f64) -> Response {
    let (rect, r) = ui.allocate_exact_size(
        vec2(width, ui.spacing().interact_size.y.min(height + 8.0)),
        Sense::hover(),
    );
    let rect = Rect::from_center_size(rect.center(), vec2(width, height));
    paint_bar(ui, colors, rect, frac);
    r
}

/// Paints a size bar into `rect`.
pub fn paint_bar(ui: &Ui, colors: &Colors, rect: Rect, frac: f64) {
    let p = ui.painter();
    let radius = CornerRadius::same(RADIUS_SM);
    p.rect_filled(rect, radius, colors.fg(Token::BarEmpty));
    #[expect(clippy::cast_possible_truncation, reason = "a width on screen")]
    let w = rect.width() * frac.clamp(0.0, 1.0) as f32;
    if w > 0.0 {
        let filled = Rect::from_min_size(rect.min, vec2(w.max(1.0), rect.height()));
        p.rect_filled(filled, radius, colors.fg(Token::BarFill));
    }
}

/// A hairline across `ui`'s width in `color`, with `gap` above and below.
pub fn rule(ui: &mut Ui, color: Color32, gap: f32) {
    ui.add_space(gap);
    let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), 1.0), Sense::hover());
    ui.painter()
        .hline(rect.x_range(), rect.center().y, Stroke::new(1.0, color));
    ui.add_space(gap);
}

/// A sheet: the list's, the map's or a review view's, on `surface` with a hairline edge.
pub fn sheet(colors: &Colors) -> Frame {
    Frame::new()
        .fill(colors.chrome.surface)
        .stroke(Stroke::new(1.0, colors.chrome.divider))
        .corner_radius(CornerRadius::same(RADIUS_MD))
        .inner_margin(Margin::same(8))
}

/// The details sheet: raised, with more room.
pub fn raised_sheet(colors: &Colors) -> Frame {
    Frame::new()
        .fill(colors.chrome.raised)
        .stroke(Stroke::new(1.0, colors.chrome.divider))
        .corner_radius(CornerRadius::same(RADIUS_MD))
        .inner_margin(Margin::same(18))
}
