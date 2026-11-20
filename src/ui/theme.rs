//! Colours and the egui style.

use egui::{Color32, CornerRadius, Stroke, vec2};

pub const BG: Color32 = Color32::from_rgb(0x0d, 0x11, 0x17);
pub const PANEL: Color32 = Color32::from_rgb(0x16, 0x1b, 0x22);
pub const PANEL_2: Color32 = Color32::from_rgb(0x21, 0x26, 0x2d);
pub const BORDER: Color32 = Color32::from_rgb(0x30, 0x36, 0x3d);
pub const GRID: Color32 = Color32::from_rgb(0x5a, 0x63, 0x6e);
pub const TEXT: Color32 = Color32::from_rgb(0xc9, 0xd1, 0xd9);
pub const MUTED: Color32 = Color32::from_rgb(0x8b, 0x94, 0x9e);
pub const ACCENT: Color32 = Color32::from_rgb(0x58, 0xa6, 0xff);
pub const GREEN: Color32 = Color32::from_rgb(0x3f, 0xb9, 0x50);
pub const RED: Color32 = Color32::from_rgb(0xf8, 0x51, 0x49);
pub const AMBER: Color32 = Color32::from_rgb(0xd2, 0x99, 0x22);
pub const CURRENT_C: Color32 = Color32::from_rgb(0xf1, 0xe0, 0x5a);
pub const POWER_C: Color32 = Color32::from_rgb(0x58, 0xa6, 0xff);
pub const MARKER_C: Color32 = Color32::from_rgb(0xd2, 0xa8, 0xff);
pub const BTN_GREEN: Color32 = Color32::from_rgb(0x23, 0x86, 0x36);
pub const BTN_RED: Color32 = Color32::from_rgb(0xb6, 0x23, 0x24);
pub const BTN_AMBER: Color32 = Color32::from_rgb(0x9e, 0x6a, 0x03);
pub const BTN_BLUE: Color32 = Color32::from_rgb(0x1f, 0x6f, 0xeb);

pub fn apply(ctx: &egui::Context) {
    // always our dark theme, even when the system (e.g. Windows) is in light mode
    ctx.set_theme(egui::ThemePreference::Dark);
    let mut v = egui::Visuals::dark();
    v.panel_fill = BG;
    v.window_fill = PANEL;
    v.window_stroke = Stroke::new(1.0, BORDER);
    v.extreme_bg_color = BG;
    v.faint_bg_color = PANEL;
    v.code_bg_color = PANEL_2;
    v.override_text_color = Some(TEXT);
    v.hyperlink_color = ACCENT;
    v.selection.bg_fill = BTN_BLUE;
    v.selection.stroke = Stroke::new(1.0, Color32::WHITE);
    v.window_corner_radius = CornerRadius::same(10);
    v.menu_corner_radius = CornerRadius::same(8);
    let w = &mut v.widgets;
    w.noninteractive.bg_fill = PANEL;
    w.noninteractive.weak_bg_fill = PANEL;
    w.noninteractive.bg_stroke = Stroke::new(1.0, BORDER);
    w.noninteractive.fg_stroke = Stroke::new(1.0, TEXT);
    w.inactive.bg_fill = PANEL_2;
    w.inactive.weak_bg_fill = PANEL_2;
    w.inactive.bg_stroke = Stroke::new(1.0, BORDER);
    w.inactive.fg_stroke = Stroke::new(1.0, TEXT);
    w.hovered.bg_fill = BORDER;
    w.hovered.weak_bg_fill = BORDER;
    w.hovered.bg_stroke = Stroke::new(1.0, MUTED);
    w.hovered.fg_stroke = Stroke::new(1.5, Color32::WHITE);
    w.active.bg_fill = PANEL;
    w.active.weak_bg_fill = PANEL;
    w.active.bg_stroke = Stroke::new(1.0, ACCENT);
    w.open.bg_fill = PANEL_2;
    w.open.weak_bg_fill = PANEL_2;
    w.open.bg_stroke = Stroke::new(1.0, ACCENT);
    for s in [&mut w.noninteractive, &mut w.inactive, &mut w.hovered, &mut w.active, &mut w.open] {
        s.corner_radius = CornerRadius::same(6);
        s.expansion = 0.0;
    }
    ctx.set_visuals(v);
    ctx.all_styles_mut(|s| {
        use egui::{FontId, TextStyle};
        s.spacing.item_spacing = vec2(8.0, 6.0);
        s.spacing.button_padding = vec2(10.0, 5.0);
        s.spacing.interact_size = vec2(40.0, 26.0);
        s.spacing.combo_height = 300.0;
        s.text_styles.insert(TextStyle::Body, FontId::proportional(13.5));
        s.text_styles.insert(TextStyle::Button, FontId::proportional(13.5));
        s.text_styles.insert(TextStyle::Monospace, FontId::monospace(12.5));
        s.text_styles.insert(TextStyle::Heading, FontId::proportional(18.0));
        s.text_styles.insert(TextStyle::Small, FontId::proportional(11.0));
    });
}
