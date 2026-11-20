//! The current and power plots and the overview strip.
//!
//! Drawing is decoupled from the data size: only the visible range is reduced
//! to about two points per pixel column (min/max, so spikes stay visible).

use std::time::Instant;

use egui::{Align2, CursorIcon, RichText, Stroke, Ui};
use egui_plot::{HLine, Line, Plot, PlotBounds, PlotPoint, PlotPoints, Span, Text, VLine};

use super::{AnalyzerApp, Grab, Scope};
use crate::fmt;
use crate::ui::theme::{ACCENT, AMBER, CURRENT_C, GRID, MARKER_C, MUTED, POWER_C, RED};
use crate::ui::widgets::plot_header;

impl AnalyzerApp {
    pub(super) fn plots(&mut self, ui: &mut Ui) {
        let width_px = ui.available_width().max(100.0);
        let ov_h = if self.s.show_overview { 84.0 } else { 0.0 };
        let ov_space = if self.s.show_overview { ov_h + 8.0 } else { 0.0 };
        let header_h = 26.0; // header row + spacing

        // Data for the main plots: visible range plus half a screen on each side,
        // so panning never shows an empty edge for a frame.
        let t0 = Instant::now();
        let span = (self.view.1 - self.view.0).max(1e-9);
        let (a, b) = self.store.index_range(self.view.0 - span * 0.5, self.view.1 + span * 0.5);
        let buckets = (width_px as usize) * 2;
        let filtered = self.store.filter_active();
        let with_raw = filtered && self.s.show_raw;
        let mut decim = std::mem::take(&mut self.decim);
        let mut decim_raw = std::mem::take(&mut self.decim_raw);
        self.store.decimate(self.store.series(true), a, b, buckets, &mut decim);
        decim_raw.clear();
        if with_raw {
            self.store.decimate(&self.store.raw, a, b, buckets, &mut decim_raw);
        }
        let (va, vb) = self.store.index_range(self.view.0, self.view.1);
        // Y range follows what is drawn (raw is the wider one when it is shown)
        let g = self.store.series(!with_raw).agg(va, vb);
        let yr = if g.count > 0 {
            let lo = g.min.min(0.0);
            let hi = g.max.max(lo + 1e-3);
            let pad = (hi - lo) * 0.06;
            (lo - if lo < 0.0 { pad } else { 0.0 }, hi + pad)
        } else {
            (0.0, 10.0)
        };

        let mut hovered = false;
        let plots_h = ui.available_height()
            - ov_space
            - header_h * (1 + self.s.show_power as usize) as f32
            - if self.s.show_power { 8.0 } else { 0.0 };
        let h_i = if self.s.show_power { plots_h * 0.58 } else { plots_h };
        let desc = self.s.filter.describe(self.dt());
        let raw = with_raw.then_some(decim_raw.as_slice());
        let desc = if desc.is_empty() { desc } else { format!("filtered: {desc}") };
        plot_header(ui, "CURRENT", CURRENT_C, &desc);
        hovered |= self.main_plot(ui, 0, h_i, &decim, raw, yr);
        if self.s.show_power {
            ui.add_space(8.0);
            plot_header(ui, "POWER", POWER_C, &desc);
            let h_p = ui.available_height() - ov_space;
            hovered |= self.main_plot(ui, 1, h_p, &decim, raw, yr);
        }
        if self.s.show_overview {
            ui.add_space(8.0);
            hovered |= self.overview(ui, ov_h, width_px);
        }
        if !hovered && self.grab == Grab::None {
            self.hover_x = None;
        }
        if std::env::var_os("ANALIZATOR_DEBUG").is_some() {
            eprintln!(
                "plots: {:.2} ms, {} points, view {:.0} samples",
                t0.elapsed().as_secs_f64() * 1e3,
                decim.len(),
                self.view.1 - self.view.0
            );
        }
        self.decim = decim;
        self.decim_raw = decim_raw;
    }

    /// Current (id 0) or power (id 1) plot. Returns true if hovered.
    fn main_plot(
        &mut self,
        ui: &mut Ui,
        id: usize,
        height: f32,
        decim: &[(f64, f64)],
        raw: Option<&[(f64, f64)]>,
        yr: (f64, f64),
    ) -> bool {
        let scale = self.xscale();
        let factor = if id == 0 { 1e-6 } else { self.voltage.max(1e-12) * 1e-6 };
        let (unit, color) = if id == 0 { ("A", CURRENT_C) } else { ("W", POWER_C) };

        // ----- selection dragging (decided before the plot so it can disable panning)
        let (pointer, pressed, down, shift) = ui.input(|i| {
            (i.pointer.interact_pos(), i.pointer.primary_pressed(), i.pointer.primary_down(), i.modifiers.shift)
        });
        // where the button went down (press and moves can arrive in the same frame)
        let press_pos = if pressed { ui.input(|i| i.pointer.press_origin()).or(pointer) } else { None };
        if !down && self.grab != Grab::None {
            if matches!(self.grab, Grab::Marker(_)) {
                self.sort_markers();
            }
            self.grab = Grab::None;
        }
        let mut near_edge = false;
        if let (Some(tf), Some(pos)) = (self.last_tf[id], pointer) {
            let xv = tf.value_from_position(pos).x / scale;
            let lx = tf.position_from_point_x(self.sel.0 * scale);
            let rx = tf.position_from_point_x(self.sel.1 * scale);
            if tf.frame().contains(pos) && self.scope == Scope::Selection {
                near_edge = (pos.x - lx).abs() < 7.0 || (pos.x - rx).abs() < 7.0;
            }
            let marker_at =
                |px: f32| self.markers.iter().position(|m| (tf.position_from_point_x(m.x * scale) - px).abs() < 6.0);
            if tf.frame().contains(pos) && marker_at(pos.x).is_some() {
                near_edge = true;
            }
            if let Some(pp) = press_pos.filter(|p| tf.frame().contains(*p) && self.grab == Grab::None) {
                let xp = tf.value_from_position(pp).x / scale;
                let sel_on = self.scope == Scope::Selection;
                self.grab = if let Some(k) = marker_at(pp.x) {
                    Grab::Marker(k)
                } else if sel_on && (pp.x - lx).abs() < 7.0 {
                    Grab::Left
                } else if sel_on && (pp.x - rx).abs() < 7.0 {
                    Grab::Right
                } else if shift {
                    self.set_scope(Scope::Selection);
                    Grab::New(xp)
                } else if sel_on && xp > self.sel.0 && xp < self.sel.1 {
                    Grab::Body(xp - self.sel.0)
                } else {
                    Grab::None
                };
                self.grab_plot = id;
            }
            if self.grab != Grab::None && self.grab_plot == id && down {
                match self.grab {
                    Grab::Left => self.sel.0 = xv.min(self.sel.1 - 1.0),
                    Grab::Right => self.sel.1 = xv.max(self.sel.0 + 1.0),
                    Grab::Body(off) => {
                        let w = self.sel.1 - self.sel.0;
                        self.sel = (xv - off, xv - off + w);
                    }
                    Grab::New(anchor) => self.sel = (anchor.min(xv), anchor.max(xv).max(anchor.min(xv) + 1.0)),
                    Grab::Marker(k) => {
                        let x = self.clamp_x(xv.round());
                        if let Some(m) = self.markers.get_mut(k) {
                            m.x = x;
                        }
                    }
                    Grab::None => {}
                }
                if !matches!(self.grab, Grab::Marker(_)) {
                    self.sel_init = true;
                }
            }
        }

        let pts: Vec<[f64; 2]> = decim.iter().map(|&(x, y)| [x * scale, y * factor]).collect();
        let raw_pts: Option<Vec<[f64; 2]>> = raw.map(|r| r.iter().map(|&(x, y)| [x * scale, y * factor]).collect());
        let bounds =
            PlotBounds::from_min_max([self.view.0 * scale, yr.0 * factor], [self.view.1 * scale, yr.1 * factor]);
        let sel = (self.scope == Scope::Selection).then_some((self.sel.0 * scale, self.sel.1 * scale));
        let hover = self.hover_x.and_then(|x| self.store.nearest(x)).map(|k| self.store.x_at(k) * scale);
        let x_time = self.s.x_time;
        let dragging_sel = self.grab != Grab::None;
        let marks: Vec<(f64, String)> = self.markers.iter().map(|m| (m.x * scale, m.label.clone())).collect();
        let label_y = yr.1 * factor;
        let threshold =
            (id == 0 && self.s.show_threshold && !self.store.is_empty()).then_some(self.s.threshold_ma * 1e-3);

        let resp = Plot::new(("main", id))
            .height(height.max(60.0))
            .show_x(false)
            .show_y(false)
            .allow_zoom(false)
            .allow_scroll(false)
            .allow_boxed_zoom(false)
            .allow_double_click_reset(false)
            .allow_axis_zoom_drag(false)
            .allow_drag([!dragging_sel, false])
            .y_axis_min_width(72.0)
            .grid_color(GRID)
            .x_axis_formatter(move |m, r| fmt::axis_label_edge(m.value, r, x_time))
            .y_axis_formatter(move |m, _| fmt::si_digits(m.value, unit, 3))
            .show(ui, move |pu| {
                pu.set_plot_bounds(bounds);
                if let Some((l, r)) = sel {
                    pu.span(Span::new("", l..=r).fill(RED.gamma_multiply(0.12)).border(Stroke::new(1.5, RED)));
                }
                if let Some(rp) = raw_pts {
                    pu.line(Line::new("", PlotPoints::new(rp)).color(color.gamma_multiply(0.28)).width(1.0));
                }
                pu.line(Line::new("", PlotPoints::new(pts)).color(color).width(1.0).fill(0.0).fill_alpha(0.10));
                if let Some(t) = threshold {
                    pu.hline(
                        HLine::new("", t)
                            .color(AMBER.gamma_multiply(0.7))
                            .width(1.0)
                            .style(egui_plot::LineStyle::dashed_dense()),
                    );
                }
                for (mx, label) in marks {
                    pu.vline(VLine::new("", mx).color(MARKER_C).width(1.5));
                    if id == 0 {
                        let text = RichText::new(format!(" {label}")).size(11.5).color(MARKER_C);
                        pu.text(Text::new("", PlotPoint::new(mx, label_y), text).anchor(Align2::LEFT_TOP));
                    }
                }
                if let Some(hx) = hover {
                    pu.vline(VLine::new("", hx).color(MUTED).width(1.0).style(egui_plot::LineStyle::dashed_loose()));
                }
            });

        let tf = resp.transform;
        if id == 0 && self.store.is_empty() {
            self.empty_hint(ui, *tf.frame());
        }
        self.last_tf[id] = Some(tf);
        let r = &resp.response;

        // pan by dragging (handled by egui_plot) -> read back the new x range
        let b = tf.bounds();
        let nv = (b.min()[0] / scale, b.max()[0] / scale);
        let eps = (self.view.1 - self.view.0).abs() * 1e-9;
        if (nv.0 - self.view.0).abs() > eps || (nv.1 - self.view.1).abs() > eps {
            self.view = nv;
            self.s.follow = false;
        }

        // mouse wheel zooms around the pointer, horizontal scroll pans
        if r.hovered() {
            let (d, zoom_delta) = ui.input(|i| (i.smooth_scroll_delta, i.zoom_delta()));
            let pos = r.hover_pos();
            let span = self.view.1 - self.view.0;
            let mut factor = 1.0;
            if d.y != 0.0 {
                factor *= (-(d.y as f64) * 0.0025).exp();
            }
            if zoom_delta != 1.0 {
                factor /= zoom_delta as f64;
            }
            if factor != 1.0 {
                let c = pos.map(|p| tf.value_from_position(p).x / scale).unwrap_or((self.view.0 + self.view.1) / 2.0);
                let new_span = (span * factor).max(10.0);
                let f = new_span / span;
                self.view = (c - (c - self.view.0) * f, c + (self.view.1 - c) * f);
                self.s.follow = false;
            }
            if d.x != 0.0 {
                let shift = -(d.x as f64) / tf.frame().width() as f64 * span;
                self.view = (self.view.0 + shift, self.view.1 + shift);
                self.s.follow = false;
            }
        }
        if r.double_clicked() {
            self.fit();
        }
        // right-click menu
        if r.secondary_clicked()
            && let Some(p) = r.interact_pointer_pos()
        {
            let near = self.markers.iter().position(|m| (tf.position_from_point_x(m.x * scale) - p.x).abs() < 6.0);
            self.ctx_menu = Some((tf.value_from_position(p).x / scale, near));
        }
        if let Some((cx, near)) = self.ctx_menu {
            let mut action = 0;
            let has_markers = !self.markers.is_empty();
            let near_label = near.and_then(|k| self.markers.get(k)).map(|m| m.label.clone());
            r.context_menu(|ui| {
                if ui.button("Add marker here").clicked() {
                    action = 1;
                }
                if let Some(l) = &near_label
                    && ui.button(format!("Delete marker “{l}”")).clicked()
                {
                    action = 2;
                }
                if has_markers && ui.button("Delete all markers").clicked() {
                    action = 3;
                }
                ui.separator();
                if ui.button("Fit all data").clicked() {
                    action = 4;
                }
                if action != 0 {
                    ui.close();
                }
            });
            match action {
                1 if !self.store.is_empty() => {
                    // added on purpose from the menu: let the user type a name right away
                    self.focus_marker = Some(self.add_marker(cx));
                    self.focus_tries = 0;
                }
                2 => {
                    if let Some(k) = near.filter(|k| *k < self.markers.len()) {
                        self.markers.remove(k);
                    }
                }
                3 => self.markers.clear(),
                4 => self.fit(),
                _ => {}
            }
            if action != 0 {
                self.ctx_menu = None;
            }
        }
        if near_edge || matches!(self.grab, Grab::Left | Grab::Right) {
            ui.ctx().set_cursor_icon(CursorIcon::ResizeHorizontal);
        } else if matches!(self.grab, Grab::Body(_)) {
            ui.ctx().set_cursor_icon(CursorIcon::Grabbing);
        }
        match r.hover_pos() {
            Some(p) => {
                self.hover_x = Some(tf.value_from_position(p).x / scale);
                true
            }
            None => false,
        }
    }

    fn overview(&mut self, ui: &mut Ui, height: f32, width_px: f32) -> bool {
        let scale = self.xscale();
        let mut decim = Vec::new();
        let series = self.store.series(true);
        self.store.decimate(series, 0, self.store.len(), width_px as usize, &mut decim);
        let pts: Vec<[f64; 2]> = decim.iter().map(|&(x, y)| [x * scale, y * 1e-6]).collect();
        let (x0, x1) = if self.store.is_empty() { self.view } else { (self.store.first_x(), self.store.last_x()) };
        let pad = ((x1 - x0) * 0.01).max(1.0);
        let ymax = if self.store.is_empty() { 1e-5 } else { series.total.max.max(1.0) * 1e-6 * 1.1 };
        let ymin = if self.store.is_empty() { 0.0 } else { series.total.min.min(0.0) * 1e-6 };
        let bounds = PlotBounds::from_min_max([(x0 - pad) * scale, ymin], [(x1 + pad) * scale, ymax]);
        let view = (self.view.0 * scale, self.view.1 * scale);
        let hover = self.hover_x.and_then(|x| self.store.nearest(x)).map(|k| self.store.x_at(k) * scale);
        let x_time = self.s.x_time;
        let marks: Vec<f64> = self.markers.iter().map(|m| m.x * scale).collect();
        let resp = Plot::new("overview")
            .height(height)
            .show_x(false)
            .show_y(false)
            .allow_zoom(false)
            .allow_scroll(false)
            .allow_drag(false)
            .allow_boxed_zoom(false)
            .allow_double_click_reset(false)
            .allow_axis_zoom_drag(false)
            .show_grid([true, false])
            .grid_color(GRID)
            .y_axis_min_width(72.0)
            .x_axis_formatter(move |m, r| fmt::axis_label_edge(m.value, r, x_time))
            .y_axis_formatter(|_, _| String::new())
            .show(ui, move |pu| {
                pu.set_plot_bounds(bounds);
                pu.line(Line::new("", PlotPoints::new(pts)).color(CURRENT_C.gamma_multiply(0.75)).width(1.0));
                pu.span(
                    Span::new("", view.0..=view.1).fill(ACCENT.gamma_multiply(0.18)).border(Stroke::new(1.0, ACCENT)),
                );
                for mx in marks {
                    pu.vline(VLine::new("", mx).color(MARKER_C.gamma_multiply(0.8)).width(1.0));
                }
                if let Some(hx) = hover {
                    pu.vline(VLine::new("", hx).color(MUTED).width(1.0));
                }
            });
        let r = &resp.response;
        if (r.dragged() || r.clicked())
            && !self.store.is_empty()
            && let Some(p) = r.interact_pointer_pos()
        {
            let xv = resp.transform.value_from_position(p).x / scale;
            let w = self.view.1 - self.view.0;
            self.view = (xv - w / 2.0, xv + w / 2.0);
            self.s.follow = false;
        }
        if r.hovered() {
            ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
        }
        r.hovered()
    }

    /// What to do first, shown over the empty current plot.
    fn empty_hint(&self, ui: &Ui, rect: egui::Rect) {
        let text = match self.mode {
            super::Mode::Connected => "Press Record (Ctrl+R) to start measuring",
            super::Mode::Connecting => "Connecting…",
            super::Mode::Reconnecting => "Waiting for the device to come back…",
            _ => "Choose a device and press Connect — or open a CSV file (Ctrl+O, or drop it here)",
        };
        ui.painter().text(rect.center(), Align2::CENTER_CENTER, text, egui::FontId::proportional(15.0), MUTED);
    }
}
