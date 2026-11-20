//! Small reusable widgets.

use egui::{Color32, CornerRadius, Frame, Margin, RichText, Stroke, Ui, vec2};

use super::theme::{ACCENT, BG, BORDER, MUTED, PANEL};

/// Button with an optional coloured fill. Returns true when clicked.
pub fn button(ui: &mut Ui, enabled: bool, text: &str, fill: Option<Color32>, tip: &str, min_w: f32) -> bool {
    button_why(ui, enabled, text, fill, tip, "", min_w)
}

/// Like [`button`]; `why_disabled` is shown on hover while it is disabled.
pub fn button_why(
    ui: &mut Ui,
    enabled: bool,
    text: &str,
    fill: Option<Color32>,
    tip: &str,
    why_disabled: &str,
    min_w: f32,
) -> bool {
    let mut rt = RichText::new(text).strong();
    if fill.is_some() {
        rt = rt.color(Color32::WHITE);
    }
    let mut b = egui::Button::new(rt).min_size(vec2(min_w, 30.0)).corner_radius(CornerRadius::same(6));
    if let Some(f) = fill {
        b = b.fill(f).stroke(Stroke::new(1.0, f.lerp_to_gamma(Color32::WHITE, 0.15)));
    }
    let mut r = ui.add_enabled(enabled, b);
    if !tip.is_empty() {
        r = r.on_hover_text(tip);
    }
    if !why_disabled.is_empty() {
        r = r.on_disabled_hover_text(why_disabled);
    }
    r.clicked()
}

/// Thin vertical separator for toolbars.
pub fn vsep(ui: &mut Ui) {
    ui.add_space(4.0);
    let (rect, _) = ui.allocate_exact_size(vec2(1.0, 24.0), egui::Sense::hover());
    ui.painter().rect_filled(rect, 0.0, BORDER);
    ui.add_space(4.0);
}

/// Collapsible sidebar section (open/closed state is remembered).
pub fn section(ui: &mut Ui, title: &str, add: impl FnOnce(&mut Ui)) {
    egui::CollapsingHeader::new(RichText::new(title).size(11.0).strong().color(MUTED))
        .id_salt(title)
        .default_open(true)
        .show_unindented(ui, |ui| card(ui, add));
}

/// Rounded panel.
pub fn card(ui: &mut Ui, add: impl FnOnce(&mut Ui)) {
    ui.add_space(2.0);
    Frame::new()
        .fill(PANEL)
        .stroke(Stroke::new(1.0, BORDER))
        .corner_radius(CornerRadius::same(8))
        .inner_margin(Margin::same(12))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            add(ui);
        });
}

/// Statistic tile: small caption, big value.
pub fn tile(ui: &mut Ui, title: &str, value: &str, color: Color32, width: f32) {
    Frame::new()
        .fill(BG)
        .stroke(Stroke::new(1.0, BORDER))
        .corner_radius(CornerRadius::same(8))
        .inner_margin(Margin::symmetric(10, 7))
        .show(ui, |ui| {
            ui.set_width(width);
            ui.vertical(|ui| {
                ui.spacing_mut().item_spacing.y = 1.0;
                ui.label(RichText::new(title).size(10.0).strong().color(MUTED));
                ui.label(RichText::new(value).size(17.0).strong().color(color));
            });
        });
}

/// Filled dot (the "●" glyph is missing from the default UI font).
pub fn dot(ui: &mut Ui, color: Color32) {
    let (rect, _) = ui.allocate_exact_size(vec2(10.0, 10.0), egui::Sense::hover());
    ui.painter().circle_filled(rect.center(), 4.0, color);
}

/// "● CURRENT  filtered: …" above a plot.
pub fn plot_header(ui: &mut Ui, title: &str, color: Color32, note: &str) {
    ui.horizontal(|ui| {
        ui.add_space(74.0);
        dot(ui, color);
        ui.label(RichText::new(title).strong().size(11.5).color(MUTED));
        if !note.is_empty() {
            ui.add_space(6.0);
            ui.label(RichText::new(note).size(11.5).color(ACCENT));
        }
    });
}

/// Muted label in a form row.
pub fn caption(ui: &mut Ui, text: &str) -> egui::Response {
    ui.label(RichText::new(text).color(MUTED))
}

/// Small muted explanatory text.
pub fn hint(ui: &mut Ui, text: impl Into<String>) {
    ui.label(RichText::new(text.into()).size(11.5).color(MUTED));
}
