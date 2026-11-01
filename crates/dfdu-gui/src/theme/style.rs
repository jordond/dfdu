//! The window's look beyond the palette's tokens, after the dfdu design system's Terminal Dark
//! and Terminal Light: the bundled fonts (IBM Plex Sans for the interface, JetBrains Mono for
//! data), the layers a window has and a terminal has not ([`Chrome`]: the window background,
//! sheets, raised surfaces, hairlines, the accent fill and what is drawn on it), and egui's
//! visuals and spacing made from them.

#![allow(
    dead_code,
    reason = "tokens the review views and dialogs are moving to"
)]

use egui::{
    Color32, CornerRadius, FontData, FontDefinitions, FontFamily, FontId, Margin, Shadow, Stroke,
    TextStyle, Visuals,
};

/// IBM Plex Sans and JetBrains Mono, under the SIL Open Font License (`fonts/*-OFL.txt`).
const SANS: &[u8] = include_bytes!("../../fonts/IBMPlexSans-Regular.ttf");
const SANS_MEDIUM: &[u8] = include_bytes!("../../fonts/IBMPlexSans-Medium.ttf");
const SANS_SEMIBOLD: &[u8] = include_bytes!("../../fonts/IBMPlexSans-SemiBold.ttf");
const MONO: &[u8] = include_bytes!("../../fonts/JetBrainsMono-Regular.ttf");
const MONO_SEMIBOLD: &[u8] = include_bytes!("../../fonts/JetBrainsMono-SemiBold.ttf");

/// The font families beside egui's `Proportional` (Plex Sans) and `Monospace` (JetBrains Mono):
/// egui has no weights, so each weight is a family of its own.
pub const SANS_MEDIUM_FAMILY: &str = "sans-medium";
pub const SANS_SEMIBOLD_FAMILY: &str = "sans-semibold";
pub const MONO_SEMIBOLD_FAMILY: &str = "mono-semibold";

/// Corner radii: key caps, checkboxes, bars, tiles and badges; buttons, fields and menus;
/// dialogs.
pub const RADIUS_SM: u8 = 3;
pub const RADIUS_MD: u8 = 5;
pub const RADIUS_LG: u8 = 8;

/// The 4px grid.
pub const SPACE_1: f32 = 4.0;
pub const SPACE_2: f32 = 8.0;
pub const SPACE_3: f32 = 12.0;
pub const SPACE_4: f32 = 16.0;
pub const SPACE_6: f32 = 24.0;
pub const SPACE_8: f32 = 32.0;

/// Fixed heights: a row of a list, a control, the toolbar, the tab bar and the status bar.
pub const ROW: f32 = 24.0;
pub const CONTROL: f32 = 28.0;
pub const TOOLBAR: f32 = 48.0;
pub const TAB_BAR: f32 = 32.0;
pub const STATUS_BAR: f32 = 40.0;

/// The colours of a window's layers, which a terminal leaves to itself.
#[derive(Clone, Copy, Debug)]
pub struct Chrome {
    /// The window, its toolbar, tab bar and status bar, and the gaps between sheets.
    pub bg: Color32,
    /// Sheets: the list, the map, the review views.
    pub surface: Color32,
    /// Menus, dialogs, fields, secondary buttons and the details sheet.
    pub raised: Color32,
    /// A row or tile under the pointer.
    pub hover: Color32,
    /// Hairlines between rows and around sheets; never a control's only edge.
    pub divider: Color32,
    /// Text on an accent fill.
    pub on_accent: Color32,
    /// Text on the delete button's fill.
    pub on_danger: Color32,
    /// Text on a treemap tile's category fill.
    pub on_tile: Color32,
    /// Behind a modal dialog.
    pub scrim: Color32,
    /// The start screen's background grid.
    pub grid: Color32,
    /// Menus and the palette, then dialogs.
    pub shadow_menu: Color32,
    pub shadow_dialog: Color32,
}

impl Chrome {
    pub const DARK: Chrome = Chrome {
        bg: Color32::from_rgb(0x1f, 0x23, 0x2b),
        surface: Color32::from_rgb(0x26, 0x2b, 0x35),
        raised: Color32::from_rgb(0x2e, 0x34, 0x40),
        hover: Color32::from_rgb(0x2b, 0x31, 0x3c),
        divider: Color32::from_rgb(0x36, 0x3d, 0x4a),
        on_accent: Color32::from_rgb(0x1d, 0x21, 0x28),
        on_danger: Color32::from_rgb(0x1d, 0x21, 0x28),
        on_tile: Color32::from_rgb(0x1d, 0x21, 0x28),
        scrim: Color32::from_rgba_unmultiplied_const(10, 12, 16, 158),
        grid: Color32::from_rgba_unmultiplied_const(136, 192, 208, 20),
        shadow_menu: Color32::from_rgba_unmultiplied_const(0, 0, 0, 115),
        shadow_dialog: Color32::from_rgba_unmultiplied_const(0, 0, 0, 140),
    };

    pub const LIGHT: Chrome = Chrome {
        bg: Color32::from_rgb(0xf1, 0xf3, 0xf6),
        surface: Color32::from_rgb(0xff, 0xff, 0xff),
        raised: Color32::from_rgb(0xff, 0xff, 0xff),
        hover: Color32::from_rgb(0xf3, 0xf5, 0xf8),
        divider: Color32::from_rgb(0xdd, 0xe1, 0xe8),
        on_accent: Color32::WHITE,
        on_danger: Color32::WHITE,
        on_tile: Color32::WHITE,
        scrim: Color32::from_rgba_unmultiplied_const(31, 35, 41, 97),
        grid: Color32::from_rgba_unmultiplied_const(27, 107, 140, 20),
        shadow_menu: Color32::from_rgba_unmultiplied_const(20, 24, 32, 36),
        shadow_dialog: Color32::from_rgba_unmultiplied_const(20, 24, 32, 56),
    };

    pub fn of(theme: egui::Theme) -> Chrome {
        match theme {
            egui::Theme::Dark => Chrome::DARK,
            egui::Theme::Light => Chrome::LIGHT,
        }
    }
}

/// Bundles the fonts: Plex Sans for egui's proportional text, JetBrains Mono for its
/// monospace, and a family per heavier weight; egui's own fonts stay behind them for the
/// symbols they lack.
pub fn fonts(ctx: &egui::Context) {
    let mut defs = FontDefinitions::default();
    let mut add = |name: &str, bytes: &'static [u8]| {
        defs.font_data
            .insert(name.to_owned(), FontData::from_static(bytes).into());
    };
    add("plex-sans", SANS);
    add("plex-sans-medium", SANS_MEDIUM);
    add("plex-sans-semibold", SANS_SEMIBOLD);
    add("jetbrains-mono", MONO);
    add("jetbrains-mono-semibold", MONO_SEMIBOLD);
    let fallback = defs
        .families
        .get(&FontFamily::Proportional)
        .cloned()
        .unwrap_or_default();
    let family = |first: &str| {
        let mut list = vec![first.to_owned()];
        list.extend(fallback.iter().cloned());
        list
    };
    defs.families
        .insert(FontFamily::Proportional, family("plex-sans"));
    defs.families
        .insert(FontFamily::Monospace, family("jetbrains-mono"));
    for (name, font) in [
        (SANS_MEDIUM_FAMILY, "plex-sans-medium"),
        (SANS_SEMIBOLD_FAMILY, "plex-sans-semibold"),
        (MONO_SEMIBOLD_FAMILY, "jetbrains-mono-semibold"),
    ] {
        defs.families
            .insert(FontFamily::Name(name.into()), family(font));
    }
    ctx.set_fonts(defs);
}

/// The text styles: `ui-body` 13, `ui-label` 12 (buttons), `ui-caption` 11, `ui-title` 15,
/// `data-row` 13 in mono.
pub fn text_styles(style: &mut egui::Style) {
    let medium = FontFamily::Name(SANS_MEDIUM_FAMILY.into());
    let semibold = FontFamily::Name(SANS_SEMIBOLD_FAMILY.into());
    style.text_styles = [
        (TextStyle::Body, FontId::new(13.0, FontFamily::Proportional)),
        (TextStyle::Button, FontId::new(12.0, medium)),
        (
            TextStyle::Small,
            FontId::new(11.0, FontFamily::Proportional),
        ),
        (TextStyle::Heading, FontId::new(15.0, semibold)),
        (
            TextStyle::Monospace,
            FontId::new(13.0, FontFamily::Monospace),
        ),
    ]
    .into();
}

/// The spacing of a dense desktop layout on the 4px grid: controls 28 high, menus with room.
pub fn spacing(style: &mut egui::Style) {
    let s = &mut style.spacing;
    s.item_spacing = egui::vec2(SPACE_2, SPACE_1);
    s.button_padding = egui::vec2(SPACE_3, 5.0);
    s.interact_size = egui::vec2(24.0, 24.0);
    s.menu_margin = Margin::same(6);
    s.window_margin = Margin::same(16);
    s.icon_width = 14.0;
    s.icon_width_inner = 8.0;
    s.icon_spacing = 6.0;
    s.text_edit_width = 220.0;
    s.combo_height = 300.0;
    style.interaction.tooltip_delay = 0.4;
}

/// egui's visuals in the tokens' colours: the window in `bg`, controls outlined in `border`
/// on raised fills, hairlines in `divider`, the accent for the active control, the selection
/// in `cursor`.
#[expect(
    clippy::too_many_arguments,
    reason = "each token a role of egui's visuals takes"
)]
pub fn visuals(
    mut v: Visuals,
    chrome: &Chrome,
    fg: Color32,
    border: Color32,
    focus: Color32,
    accent: Color32,
    cursor: (Color32, Color32),
    hyperlink: Color32,
) -> Visuals {
    let md = CornerRadius::same(RADIUS_MD);
    v.override_text_color = Some(fg);
    v.panel_fill = chrome.bg;
    v.window_fill = chrome.raised;
    v.window_stroke = Stroke::new(1.0, border);
    v.window_corner_radius = CornerRadius::same(RADIUS_LG);
    v.menu_corner_radius = md;
    v.window_shadow = Shadow {
        offset: [0, 16],
        blur: 40,
        spread: 0,
        color: chrome.shadow_dialog,
    };
    v.popup_shadow = Shadow {
        offset: [0, 4],
        blur: 12,
        spread: 0,
        color: chrome.shadow_menu,
    };
    v.extreme_bg_color = chrome.raised;
    v.text_edit_bg_color = Some(chrome.raised);
    v.faint_bg_color = chrome.hover;
    v.code_bg_color = chrome.hover;
    v.hyperlink_color = hyperlink;
    v.selection.bg_fill = cursor.1;
    v.selection.stroke = Stroke::new(1.0, focus);
    v.striped = false;
    v.button_frame = true;
    v.indent_has_left_vline = false;
    let w = &mut v.widgets;
    w.noninteractive.bg_fill = chrome.surface;
    w.noninteractive.weak_bg_fill = chrome.surface;
    w.noninteractive.bg_stroke = Stroke::new(1.0, chrome.divider);
    w.noninteractive.fg_stroke = Stroke::new(1.0, fg);
    w.noninteractive.corner_radius = md;
    w.inactive.bg_fill = chrome.raised;
    w.inactive.weak_bg_fill = chrome.raised;
    w.inactive.bg_stroke = Stroke::new(1.0, border);
    w.inactive.fg_stroke = Stroke::new(1.0, fg);
    w.inactive.corner_radius = md;
    w.inactive.expansion = 0.0;
    w.hovered.bg_fill = chrome.hover;
    w.hovered.weak_bg_fill = chrome.hover;
    w.hovered.bg_stroke = Stroke::new(1.0, focus);
    w.hovered.fg_stroke = Stroke::new(1.0, fg);
    w.hovered.corner_radius = md;
    w.hovered.expansion = 0.0;
    w.active.bg_fill = accent;
    w.active.weak_bg_fill = accent;
    w.active.bg_stroke = Stroke::new(1.0, accent);
    w.active.fg_stroke = Stroke::new(1.0, chrome.on_accent);
    w.active.corner_radius = md;
    w.active.expansion = 0.0;
    w.open.bg_fill = chrome.hover;
    w.open.weak_bg_fill = chrome.hover;
    w.open.bg_stroke = Stroke::new(1.0, focus);
    w.open.fg_stroke = Stroke::new(1.0, fg);
    w.open.corner_radius = md;
    v
}
