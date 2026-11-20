//! The right-hand panel: device settings, measurement, filter, statistics,
//! battery & sleep, markers and the device log.

use std::time::{Duration, Instant};

use egui::{Color32, RichText, Ui};

use super::{AnalyzerApp, Scope, StateSplit};
use crate::data::Agg;
use crate::filter::{FilterKind, MAX_AVG_WINDOW, MAX_MEDIAN_WINDOW};
use crate::fmt;
use crate::ui::theme::{ACCENT, AMBER, CURRENT_C, GREEN, MUTED, POWER_C, TEXT};
use crate::ui::widgets::{caption, dot, hint, section, tile};

impl AnalyzerApp {
    pub(super) fn sidebar(&mut self, ui: &mut Ui) {
        egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
            ui.add_space(8.0);
            let pctx = self.panel_ctx();
            if let Some(dev) = self.dev.as_mut()
                && let Some(title) = dev.panel_title()
            {
                section(ui, title, |ui| dev.panel_ui(ui, &pctx));
                ui.add_space(8.0);
            }
            section(ui, "MEASUREMENT", |ui| self.measurement_ui(ui));
            ui.add_space(8.0);
            section(ui, "FILTER", |ui| self.filter_ui(ui));
            ui.add_space(8.0);
            section(ui, "STATISTICS", |ui| self.statistics_ui(ui));
            ui.add_space(8.0);
            section(ui, "BATTERY & SLEEP", |ui| self.analysis_ui(ui));
            ui.add_space(8.0);
            section(ui, "MARKERS", |ui| self.markers_ui(ui));
            ui.add_space(8.0);
            section(ui, "DEVICE LOG", |ui| self.log_ui(ui));
        });
    }

    fn measurement_ui(&mut self, ui: &mut Ui) {
        let caps = self.dev.as_ref().map(|d| d.caps()).unwrap_or_default();
        egui::Grid::new("settings").num_columns(2).spacing([12.0, 8.0]).show(ui, |ui| {
            caption(ui, "DUT voltage");
            let r = ui
                .add_enabled(
                    // a source-meter device sets it, except for data loaded from a file
                    caps.supply_voltage.is_none() || (!self.recording() && !self.store.is_empty()),
                    egui::DragValue::new(&mut self.voltage)
                        .range(0.0..=30.0)
                        .speed(0.01)
                        .fixed_decimals(3)
                        .suffix(" V"),
                )
                .on_hover_text("Supply voltage of the device under test (used for power and energy)")
                .on_disabled_hover_text("Set by the connected device");
            if r.changed() && caps.supply_voltage.is_none() {
                self.s.dut_voltage = self.voltage;
            }
            ui.end_row();
            caption(ui, "Sample period");
            let r = ui
                .add_enabled(
                    caps.fixed_sample_period.is_none() && !self.recording(),
                    egui::DragValue::new(&mut self.period_ms)
                        .range(0.001..=10_000.0)
                        .speed(0.01)
                        .fixed_decimals(3)
                        .suffix(" ms"),
                )
                .on_hover_text("Time between samples (used for the time axis, charge and energy)")
                .on_disabled_hover_text("Set by the connected device");
            if r.changed() {
                self.s.sample_period_ms = self.period_ms;
                self.set_sample_period_on_device();
            }
            ui.end_row();
            caption(ui, "X axis");
            ui.horizontal(|ui| {
                ui.selectable_value(&mut self.s.x_time, true, "Time");
                ui.selectable_value(&mut self.s.x_time, false, "Sample");
            });
            ui.end_row();
            caption(ui, "Live window");
            ui.add(
                egui::DragValue::new(&mut self.s.live_window_s)
                    .range(0.1..=36_000.0)
                    .speed(0.1)
                    .fixed_decimals(1)
                    .suffix(" s"),
            )
            .on_hover_text("How much recent data is shown while following live data");
            ui.end_row();
        });
        ui.add_space(4.0);
        ui.checkbox(&mut self.s.follow, "Follow live data");
        ui.horizontal(|ui| {
            ui.checkbox(&mut self.s.show_power, "Power plot");
            ui.add_space(12.0);
            ui.checkbox(&mut self.s.show_overview, "Overview");
        });
    }

    fn filter_ui(&mut self, ui: &mut Ui) {
        let dt = self.dt();
        let mut kind = self.s.filter.kind;
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 4.0;
            ui.selectable_value(&mut kind, FilterKind::Off, "Off");
            ui.selectable_value(&mut kind, FilterKind::MovingAverage, "Average")
                .on_hover_text("Moving average: smooths noise.\nCentered window, so pulses are not shifted in time.");
            ui.selectable_value(&mut kind, FilterKind::Median, "Median")
                .on_hover_text("Median: removes short spikes and glitches but keeps sharp edges.");
            ui.selectable_value(&mut kind, FilterKind::LowPass, "Low-pass")
                .on_hover_text("First-order low-pass, like an RC filter or an oscilloscope's bandwidth limit.");
        });
        if kind != self.s.filter.kind {
            self.s.filter.kind = kind;
            self.s.filter = self.s.filter.sanitized();
            self.filter_edit = self.s.filter;
        }
        let mut apply = false;
        match kind {
            FilterKind::Off => {
                ui.label(
                    RichText::new(
                        "Raw data is shown. Filters only change the display; recordings are always saved unfiltered.",
                    )
                    .color(MUTED)
                    .size(12.0),
                );
            }
            FilterKind::MovingAverage | FilterKind::Median => {
                let max = if kind == FilterKind::Median { MAX_MEDIAN_WINDOW } else { MAX_AVG_WINDOW };
                ui.horizontal(|ui| {
                    caption(ui, "Window");
                    let r = ui.add(
                        egui::DragValue::new(&mut self.filter_edit.window).range(3..=max).speed(0.5).suffix(" samples"),
                    );
                    apply |= r.drag_stopped() || (r.changed() && !r.dragged());
                    caption(ui, &format!("= {}", fmt::si((self.filter_edit.window | 1) as f64 * dt, "s")));
                });
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 4.0;
                    for t in [1e-3, 5e-3, 20e-3, 100e-3, 1.0] {
                        let w = (((t / dt).round() as usize) | 1).clamp(3, max);
                        if ui.small_button(fmt::si_digits(t, "s", 3)).on_hover_text(format!("{w} samples")).clicked() {
                            self.filter_edit.window = w;
                            apply = true;
                        }
                    }
                });
            }
            FilterKind::LowPass => {
                let nyquist = 0.5 / dt;
                ui.horizontal(|ui| {
                    caption(ui, "Cut-off");
                    let speed = (self.filter_edit.cutoff_hz * 0.01).max(0.001);
                    let r = ui.add(
                        egui::DragValue::new(&mut self.filter_edit.cutoff_hz)
                            .range(0.001..=nyquist)
                            .speed(speed)
                            .max_decimals(3)
                            .suffix(" Hz"),
                    );
                    apply |= r.drag_stopped() || (r.changed() && !r.dragged());
                    let tau = 1.0 / (std::f64::consts::TAU * self.filter_edit.cutoff_hz.max(1e-9));
                    ui.label(RichText::new(format!("τ = {}", fmt::si(tau, "s")))).on_hover_text("Time constant (RC)");
                });
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 4.0;
                    for f in [1.0, 10.0, 50.0, 100.0, 1000.0] {
                        if f < nyquist && ui.small_button(fmt::si_digits(f, "Hz", 3)).clicked() {
                            self.filter_edit.cutoff_hz = f;
                            apply = true;
                        }
                    }
                });
            }
        }
        if apply {
            self.filter_edit.kind = self.s.filter.kind;
            self.s.filter = self.filter_edit.sanitized();
            self.filter_edit = self.s.filter;
        }
        if kind != FilterKind::Off {
            ui.checkbox(&mut self.s.show_raw, "Show raw data behind");
        }
    }

    fn statistics_ui(&mut self, ui: &mut Ui) {
        ui.horizontal(|ui| {
            caption(ui, "Range");
            let mut scope = self.scope;
            egui::ComboBox::from_id_salt("scope")
                .width(ui.available_width())
                .selected_text(match scope {
                    Scope::All => "Whole capture",
                    Scope::Selection => "Selection",
                    Scope::Visible => "Visible window",
                })
                .show_ui(ui, |ui| {
                    ui.selectable_value(&mut scope, Scope::All, "Whole capture");
                    ui.selectable_value(&mut scope, Scope::Selection, "Selection");
                    ui.selectable_value(&mut scope, Scope::Visible, "Visible window");
                })
                .response
                .on_hover_text("Selection: drag the red region's edges, or Shift+drag on a plot (Ctrl+E)");
            if scope != self.scope {
                self.set_scope(scope);
            }
        });
        if self.store.filter_active() {
            ui.checkbox(&mut self.s.stats_filtered, "Use filtered data").on_hover_text(
                "Off: statistics come from the raw samples (recommended for charge/energy).\nOn: from the filtered signal, e.g. for sustained peak current.",
            );
        }
        ui.add_space(6.0);
        self.stats_tiles(ui);
        ui.add_space(6.0);
        self.cursor_readout(ui);
    }

    /// Sample range [a, b) the statistics are about (whole capture, selection or visible window).
    pub(super) fn scope_range(&self) -> Option<(usize, usize)> {
        let n = self.store.len();
        let (a, b) = match self.scope {
            _ if n == 0 => return None,
            Scope::All => (0, n),
            Scope::Selection => self.store.index_range(self.sel.0, self.sel.1),
            Scope::Visible => self.store.index_range(self.view.0, self.view.1),
        };
        (b > a).then_some((a, b))
    }

    fn scope_agg(&self) -> Option<Agg> {
        let (a, b) = self.scope_range()?;
        Some(self.store.series(self.s.stats_filtered).agg(a, b)).filter(|g| g.count > 0)
    }

    /// Time and charge below/above the sleep threshold (cached; it needs a full scan).
    fn state_split(&mut self) -> Option<StateSplit> {
        let (a, b) = self.scope_range()?;
        let thr = self.s.threshold_ma * 1000.0;
        let key = (self.store.generation, a, b, thr.to_bits(), self.s.stats_filtered && self.store.filter_active());
        if let Some((k, t, r)) = &self.state_cache {
            // while recording, refresh at most ~4 times a second
            if *k == key
                || (self.recording() && k.3 == key.3 && k.4 == key.4 && t.elapsed() < Duration::from_millis(250))
            {
                return Some(*r);
            }
        }
        let r = self.store.series(self.s.stats_filtered).split(a, b, thr);
        self.state_cache = Some((key, Instant::now(), r));
        Some(r)
    }

    fn analysis_ui(&mut self, ui: &mut Ui) {
        ui.horizontal(|ui| {
            caption(ui, "Sleep below");
            ui.add(
                egui::DragValue::new(&mut self.s.threshold_ma)
                    .range(0.0001..=10_000.0)
                    .speed(0.01)
                    .max_decimals(4)
                    .suffix(" mA"),
            )
            .on_hover_text("Current threshold: below = sleep, at or above = active");
            ui.checkbox(&mut self.s.show_threshold, "show").on_hover_text("Draw the threshold on the current plot");
        });
        let dt = self.dt();
        if let Some(((nl, sl), (nh, sh))) = self.state_split() {
            let total_n = (nl + nh).max(1) as f64;
            let total_q = sl + sh;
            let pct = |v: f64, t: f64| if t > 0.0 { format!("{:.1} %", v / t * 100.0) } else { "—".into() };
            let avg = |s: f64, n: usize| if n > 0 { fmt::si(s / n as f64 * 1e-6, "A") } else { "—".into() };
            egui::Grid::new("states").num_columns(4).spacing([14.0, 4.0]).show(ui, |ui| {
                ui.label("");
                for h in ["time", "avg current", "charge"] {
                    ui.label(RichText::new(h).size(11.0).color(MUTED));
                }
                ui.end_row();
                for (name, col, n, sum) in [("Sleep", ACCENT, nl, sl), ("Active", AMBER, nh, sh)] {
                    ui.horizontal(|ui| {
                        dot(ui, col);
                        ui.label(RichText::new(name).color(col));
                    });
                    ui.label(pct(n as f64, total_n)).on_hover_text(fmt::duration(n as f64 * dt));
                    ui.label(avg(sum, n));
                    ui.label(pct(sum, total_q)).on_hover_text(fmt::si(sum * 1e-6 * dt / 3600.0, "Ah"));
                    ui.end_row();
                }
            });
        } else {
            caption(ui, "No data");
        }
        ui.add_space(6.0);
        ui.horizontal(|ui| {
            caption(ui, "Battery");
            ui.add(
                egui::DragValue::new(&mut self.s.battery_mah)
                    .range(0.1..=1_000_000.0)
                    .speed(1.0)
                    .max_decimals(1)
                    .suffix(" mAh"),
            )
            .on_hover_text("Battery capacity, e.g. CR2032 ≈ 220 mAh, AA ≈ 2500 mAh");
            let life = self
                .scope_agg()
                .map(|g| g.sum / g.count as f64)
                .filter(|avg| *avg > 0.0)
                .map(|avg_ua| fmt::runtime(self.s.battery_mah * 1000.0 / avg_ua));
            ui.label(RichText::new(format!("lasts ≈ {}", life.unwrap_or_else(|| "—".into()))).strong().color(GREEN));
        });
        hint(
            ui,
            format!(
                "Based on the average current of the {}; ignores self-discharge and temperature.",
                self.scope.name()
            ),
        );
    }

    fn stats_tiles(&self, ui: &mut Ui) {
        let (dt, v) = (self.dt(), self.voltage);
        let vals: [(&str, String, Color32); 10] = match self.scope_agg() {
            Some(g) => {
                let cnt = g.count as f64;
                let avg = g.sum / cnt;
                [
                    ("DURATION", fmt::duration(cnt * dt), TEXT),
                    ("SAMPLES", fmt::count(g.count), TEXT),
                    ("AVG CURRENT", fmt::si(avg * 1e-6, "A"), CURRENT_C),
                    ("RMS CURRENT", fmt::si((g.sumsq / cnt).sqrt() * 1e-6, "A"), CURRENT_C),
                    ("PEAK CURRENT", fmt::si(g.max * 1e-6, "A"), CURRENT_C),
                    ("MIN CURRENT", fmt::si(g.min * 1e-6, "A"), CURRENT_C),
                    ("AVG POWER", fmt::si(avg * v * 1e-6, "W"), POWER_C),
                    ("PEAK POWER", fmt::si(g.max * v * 1e-6, "W"), POWER_C),
                    ("CHARGE", fmt::si(g.sum * 1e-6 * dt / 3600.0, "Ah"), GREEN),
                    ("ENERGY", fmt::si(g.sum * v * 1e-6 * dt, "J"), GREEN),
                ]
            }
            None => {
                let d = || "—".to_string();
                [
                    ("DURATION", d(), TEXT),
                    ("SAMPLES", "0".into(), TEXT),
                    ("AVG CURRENT", d(), CURRENT_C),
                    ("RMS CURRENT", d(), CURRENT_C),
                    ("PEAK CURRENT", d(), CURRENT_C),
                    ("MIN CURRENT", d(), CURRENT_C),
                    ("AVG POWER", d(), POWER_C),
                    ("PEAK POWER", d(), POWER_C),
                    ("CHARGE", d(), GREEN),
                    ("ENERGY", d(), GREEN),
                ]
            }
        };
        let w = ((ui.available_width() - 6.0) / 2.0 - 22.0).max(80.0);
        egui::Grid::new("tiles").num_columns(2).spacing([6.0, 6.0]).show(ui, |ui| {
            for (k, (t, val, c)) in vals.iter().enumerate() {
                tile(ui, t, val, *c, w);
                if k % 2 == 1 {
                    ui.end_row();
                }
            }
        });
    }

    fn cursor_readout(&self, ui: &mut Ui) {
        let Some(k) = self.hover_x.and_then(|x| self.store.nearest(x)) else {
            ui.label(RichText::new("Hover a plot to read values").color(MUTED).size(12.0));
            return;
        };
        let xs = self.store.x_at(k);
        let at = if self.s.x_time { fmt::si(xs * self.dt(), "s") } else { format!("#{}", xs as i64) };
        let v = self.voltage;
        let row = |ui: &mut Ui, head: &str, i: f64, dim: f32| {
            ui.horizontal_wrapped(|ui| {
                ui.spacing_mut().item_spacing.x = 10.0;
                ui.label(RichText::new(head).monospace().color(MUTED));
                ui.label(
                    RichText::new(format!("I = {}", fmt::si(i * 1e-6, "A")))
                        .monospace()
                        .color(CURRENT_C.gamma_multiply(dim)),
                );
                ui.label(
                    RichText::new(format!("P = {}", fmt::si(i * v * 1e-6, "W")))
                        .monospace()
                        .color(POWER_C.gamma_multiply(dim)),
                );
            });
        };
        row(ui, &at, self.store.raw.v[k], 1.0);
        if self.store.filter_active() {
            row(ui, "filtered", self.store.filt.v[k], 0.8);
        }
    }

    fn log_ui(&mut self, ui: &mut Ui) {
        ui.horizontal(|ui| {
            if ui.small_button("Open logs folder").on_hover_text(self.logs_dir.display().to_string()).clicked() {
                super::files::open_folder(&self.logs_dir);
            }
            if ui.small_button("Clear").clicked() {
                self.log.clear();
            }
            if ui.small_button("Copy").on_hover_text("Copy the log to the clipboard").clicked() {
                let text: String = self.log.iter().map(|l| format!("{}  {}\n", l.time, l.text)).collect();
                ui.ctx().copy_text(text);
            }
        });
        egui::ScrollArea::vertical()
            .id_salt("log")
            .max_height((ui.available_height() - 24.0).max(160.0))
            .stick_to_bottom(true)
            .auto_shrink([false, false])
            .show(ui, |ui| {
                ui.spacing_mut().item_spacing.y = 3.0;
                let font = egui::FontId::monospace(11.5);
                for l in &self.log {
                    let mut job = egui::text::LayoutJob::default();
                    job.wrap.max_width = ui.available_width();
                    let style = |c| egui::TextFormat { font_id: font.clone(), color: c, ..Default::default() };
                    job.append(&l.time, 0.0, style(MUTED));
                    job.append(&l.text, 8.0, style(l.color));
                    ui.add(egui::Label::new(job).wrap());
                }
            });
    }
}
