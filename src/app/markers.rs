//! Markers: labelled positions in the capture (saved in the capture file).

use egui::{RichText, Ui};

use super::{AnalyzerApp, Scope};
use crate::fmt;
use crate::ui::theme::{MARKER_C, MUTED};

#[derive(Clone, Debug, PartialEq)]
pub struct Marker {
    /// Sample index.
    pub x: f64,
    pub label: String,
}

impl AnalyzerApp {
    /// Keep a position inside the recorded data.
    pub(super) fn clamp_x(&self, x: f64) -> f64 {
        if self.store.is_empty() { x } else { x.clamp(self.store.first_x(), self.store.last_x()) }
    }

    /// Add a marker; returns its index in the (sorted) list.
    pub(super) fn add_marker(&mut self, x: f64) -> usize {
        let x = self.clamp_x(x.round());
        self.marker_seq += 1;
        let k = self.markers.partition_point(|o| o.x <= x);
        self.markers.insert(k, Marker { x, label: format!("Marker {}", self.marker_seq) });
        k
    }

    pub(super) fn sort_markers(&mut self) {
        self.markers.sort_by(|a, b| a.x.total_cmp(&b.x));
    }

    /// Show a sample position centred in the plots.
    pub(super) fn go_to(&mut self, x: f64) {
        let half = (self.view.1 - self.view.0) / 2.0;
        self.view = (x - half, x + half);
        self.s.follow = false;
    }

    pub(super) fn markers_ui(&mut self, ui: &mut Ui) {
        if self.markers.is_empty() {
            ui.label(
                RichText::new(
                    "Press M while pointing at a plot, or right-click a plot and choose Add marker. \
                     While recording, M marks the current moment. Drag a marker line to move it.",
                )
                .size(12.0)
                .color(MUTED),
            );
            return;
        }
        let (time, dt) = (self.s.x_time, self.dt());
        let (mut go, mut select, mut delete) = (None, None, None);
        let n = self.markers.len();
        egui::Grid::new("markers").num_columns(3).spacing([6.0, 4.0]).show(ui, |ui| {
            for k in 0..n {
                let r = ui
                    .add_sized(
                        [130.0, 24.0],
                        egui::TextEdit::singleline(&mut self.markers[k].label).text_color(MARKER_C),
                    )
                    .on_hover_text("Click to rename");
                if self.focus_marker == Some(k) {
                    self.focus_label(ui, &r, k);
                }
                let x = self.markers[k].x;
                let t = if time { fmt::si(x * dt, "s") } else { format!("#{}", x as i64) };
                if ui
                    .add(egui::Button::new(RichText::new(t).monospace().size(12.0)).frame(false))
                    .on_hover_text("Show in plot")
                    .clicked()
                {
                    go = Some(x);
                }
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 3.0;
                    if k + 1 < n
                        && ui.small_button("➡").on_hover_text("Select from this marker to the next one").clicked()
                    {
                        select = Some(k);
                    }
                    if ui.small_button("🗙").on_hover_text("Delete marker").clicked() {
                        delete = Some(k);
                    }
                });
                ui.end_row();
            }
        });
        if let Some(x) = go {
            self.go_to(x);
        }
        if let Some(k) = select {
            self.sel = (self.markers[k].x, self.markers[k + 1].x);
            self.sel_init = true;
            self.scope = Scope::Selection;
        }
        if let Some(k) = delete {
            self.markers.remove(k);
        }
        if ui.small_button("Delete all").clicked() {
            self.markers.clear();
        }
    }

    /// Put the cursor into a new marker's name with the default text selected.
    /// The closing right-click menu can take the focus away again in the first
    /// frames, so this keeps asking for a few frames.
    fn focus_label(&mut self, ui: &Ui, r: &egui::Response, k: usize) {
        self.focus_tries += 1;
        if r.has_focus() || self.focus_tries > 20 {
            self.focus_marker = None;
            return;
        }
        r.request_focus();
        let n = self.markers[k].label.chars().count();
        if let Some(mut st) = egui::text_edit::TextEditState::load(ui.ctx(), r.id) {
            st.cursor.set_char_range(Some(egui::text::CCursorRange::two(
                egui::text::CCursor::new(0),
                egui::text::CCursor::new(n),
            )));
            st.store(ui.ctx(), r.id);
        }
        ui.ctx().request_repaint();
    }
}
