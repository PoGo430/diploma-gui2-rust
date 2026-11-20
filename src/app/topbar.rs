//! Toolbar, status bar and keyboard shortcuts.

use egui::{Align, CornerRadius, Frame, Key, KeyboardShortcut, Layout, Margin, Modifiers, RichText, Stroke, Ui, vec2};

use super::{AnalyzerApp, Confirm, FileAction, Mode, Scope};
use crate::device;
use crate::fmt;
use crate::ui::theme::{ACCENT, AMBER, BTN_AMBER, BTN_BLUE, BTN_GREEN, BTN_RED, GREEN, MUTED, RED, TEXT};
use crate::ui::widgets::{button, button_why, vsep};

const BAUD_RATES: [u32; 7] = [115_200, 230_400, 460_800, 921_600, 1_000_000, 2_000_000, 3_000_000];

impl AnalyzerApp {
    pub(super) fn top_bar(&mut self, ui: &mut Ui) {
        let ctx = ui.ctx().clone();
        let connected = self.connected();
        let idle = self.dev.is_none() && self.mode == Mode::Offline;
        let rec = self.recording();
        let busy = self.busy.is_some();
        let caps = self.dev.as_ref().map(|d| d.caps()).unwrap_or_default();
        ui.horizontal_centered(|ui| {
            ui.spacing_mut().item_spacing.x = 6.0;
            let sel = self.selected_entry().cloned();
            let label = sel.as_ref().map(|e| e.label.clone()).unwrap_or_else(|| "No device".into());
            ui.add_enabled_ui(idle, |ui| {
                let r = egui::ComboBox::from_id_salt("device")
                    .width(250.0)
                    .truncate()
                    .selected_text(label)
                    .show_ui(ui, |ui| {
                        let mut last_driver = "";
                        for e in &self.devices {
                            if !last_driver.is_empty() && e.driver != last_driver {
                                ui.separator();
                            }
                            last_driver = e.driver;
                            let r = ui.selectable_value(&mut self.device_sel, e.id.clone(), &e.label);
                            if !e.detail.is_empty() {
                                r.on_hover_text(&e.detail);
                            }
                        }
                    })
                    .response;
                r.on_hover_text(sel.as_ref().map(|e| e.detail.as_str()).filter(|d| !d.is_empty()).unwrap_or("Device"));
                if ui
                    .add(egui::Button::new("⟳").min_size(vec2(30.0, 30.0)))
                    .on_hover_text("Look for devices again")
                    .clicked()
                {
                    self.rescan();
                }
                let uses_baud = sel.as_ref().and_then(|e| device::driver(e.driver)).is_some_and(|d| d.uses_baud_rate());
                ui.add_enabled_ui(uses_baud, |ui| {
                    egui::ComboBox::from_id_salt("baud")
                        .width(96.0)
                        .selected_text(&self.baud_text)
                        .show_ui(ui, |ui| {
                            for b in BAUD_RATES {
                                ui.selectable_value(&mut self.baud_text, b.to_string(), b.to_string());
                            }
                            ui.separator();
                            ui.horizontal(|ui| {
                                ui.label("Custom:");
                                ui.add(egui::TextEdit::singleline(&mut self.baud_text).desired_width(80.0));
                            });
                        })
                        .response
                        .on_hover_text("Baud rate");
                })
                .response
                .on_disabled_hover_text("This device does not use a baud rate");
            });
            let (t, c) = match self.mode {
                Mode::Connecting | Mode::Reconnecting => ("Cancel", BTN_RED),
                _ if connected => ("Disconnect", BTN_RED),
                _ => ("Connect", BTN_GREEN),
            };
            if button(ui, true, t, Some(c), "", 104.0) {
                self.toggle_connection(&ctx);
            }
            vsep(ui);
            let ready = connected && !rec && !busy && self.mode == Mode::Connected;
            let why = if !connected { "Connect a device first" } else { "" };
            if button_why(ui, ready, "⏺  Record", Some(BTN_GREEN), "Start a recording (Ctrl+R)", why, 0.0) {
                self.start_capture();
            }
            if button(ui, connected && rec, "⏹  Stop", Some(BTN_RED), "Stop and save as CSV (Ctrl+R)", 0.0) {
                self.stop();
            }
            if button(
                ui,
                ready && caps.flash,
                "Dump flash",
                Some(BTN_AMBER),
                "Download the samples stored in the board's flash",
                0.0,
            ) {
                self.request_dump();
            }
            if button_why(ui, ready && caps.flash, "Erase flash", None, "Erase the board's flash memory", why, 0.0) {
                self.confirm = Some(Confirm::Erase);
            }
            vsep(ui);
            if button(
                ui,
                !rec && !busy,
                "Open CSV…",
                Some(BTN_BLUE),
                "Open a saved capture (Ctrl+O) — or drop a file on the window",
                0.0,
            ) {
                self.open_dialog(FileAction::Open);
            }
            let export_tip = "Save the whole capture, or the selection if one is active (Ctrl+S)";
            let why = if self.store.is_empty() { "Nothing to export yet" } else { "Stop the recording first" };
            if button_why(ui, !rec && !busy && !self.store.is_empty(), "Export CSV…", None, export_tip, why, 0.0) {
                self.open_dialog(FileAction::Export);
            }
            if button(
                ui,
                !self.store.is_empty(),
                "Fit",
                None,
                "Zoom to show all data (Ctrl+F, or double-click a plot)",
                0.0,
            ) {
                self.fit();
            }
            if button(ui, !busy, "Clear", None, "Remove all data", 0.0) {
                self.clear_data();
            }

            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if ui
                    .add(egui::Button::new(RichText::new("?").strong()).min_size(vec2(30.0, 30.0)))
                    .on_hover_text("Help and About (F1)")
                    .clicked()
                {
                    self.show_about = true;
                }
                let (text, col) = if busy {
                    ("WORKING…".to_string(), ACCENT)
                } else {
                    match self.mode {
                        Mode::Capture => ("⏺  REC".to_string(), RED),
                        Mode::Dump => ("DUMPING".to_string(), AMBER),
                        Mode::Connecting => ("CONNECTING…".to_string(), ACCENT),
                        Mode::Connected => {
                            (format!("CONNECTED · {}", self.dev.as_ref().map(|d| d.badge()).unwrap_or_default()), GREEN)
                        }
                        Mode::Offline => ("OFFLINE".to_string(), MUTED),
                        Mode::Reconnecting => ("RECONNECTING…".to_string(), AMBER),
                    }
                };
                Frame::new()
                    .fill(col.gamma_multiply(0.14))
                    .stroke(Stroke::new(1.0, col))
                    .corner_radius(CornerRadius::same(12))
                    .inner_margin(Margin::symmetric(12, 4))
                    .show(ui, |ui| {
                        ui.label(RichText::new(text).color(col).strong().size(11.5));
                    });
                if self.mode == Mode::Capture {
                    let el = self.capture_t0.elapsed().as_secs_f64();
                    let (m, s) = ((el / 60.0).floor(), el % 60.0);
                    ui.label(RichText::new(format!("{m:02}:{s:04.1}")).monospace().size(14.0).color(TEXT));
                }
            });
        });
    }

    pub(super) fn status_bar(&self, ui: &mut Ui) {
        ui.horizontal_centered(|ui| {
            let msg = self.busy.as_deref().unwrap_or(&self.status);
            ui.label(RichText::new(msg).color(MUTED));
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                ui.label(RichText::new(fmt::si_digits(self.store.bytes() as f64, "B", 3)).color(MUTED))
                    .on_hover_text("Memory used by the samples");
                ui.add_space(12.0);
                if let Some(d) = &self.dev {
                    let (rx, bad) = d.link().counts();
                    let (n_rx, n_bad) = d.counter_names();
                    let col = if bad > 0 { RED } else { MUTED };
                    ui.label(RichText::new(format!("{} {n_bad}", fmt::count(bad as usize))).color(col))
                        .on_hover_text(d.counter_help());
                    ui.label(RichText::new(format!("{} {n_rx} ·", fmt::count(rx as usize))).color(MUTED));
                    ui.add_space(12.0);
                }
                if self.recording() {
                    ui.label(RichText::new(fmt::si_digits(self.rate, "S/s", 3)).color(MUTED));
                    ui.add_space(12.0);
                }
                ui.label(RichText::new(format!("{} samples", fmt::count(self.store.len()))).color(MUTED));
            });
        });
    }

    pub(super) fn shortcuts(&mut self, ctx: &egui::Context) {
        let cmd = |k| KeyboardShortcut::new(Modifiers::COMMAND, k);
        if ctx.input_mut(|i| i.consume_shortcut(&cmd(Key::R))) {
            self.toggle_record();
        }
        if ctx.input_mut(|i| i.consume_shortcut(&cmd(Key::O))) {
            self.open_dialog(FileAction::Open);
        }
        if ctx.input_mut(|i| i.consume_shortcut(&cmd(Key::S))) {
            self.open_dialog(FileAction::Export);
        }
        if ctx.input_mut(|i| i.consume_shortcut(&cmd(Key::F))) {
            self.fit();
        }
        if ctx.input_mut(|i| i.consume_shortcut(&cmd(Key::E))) {
            let next = if self.scope == Scope::Selection { Scope::All } else { Scope::Selection };
            self.set_scope(next);
        }
        if ctx.input(|i| i.key_pressed(Key::F1)) {
            self.show_about = !self.show_about;
        }
        // M = add a marker (at the mouse, or "now" while recording); not while typing
        let typing = ctx.egui_wants_keyboard_input();
        if !typing && !self.store.is_empty() && ctx.input(|i| i.key_pressed(Key::M) && i.modifiers.is_none()) {
            let x = match self.hover_x {
                Some(h) => h,
                None if self.recording() => self.store.last_x(),
                None => (self.view.0 + self.view.1) / 2.0,
            };
            self.add_marker(x);
        }
    }

    pub(super) fn set_scope(&mut self, scope: Scope) {
        if scope == Scope::Selection && !self.sel_init {
            let span = self.view.1 - self.view.0;
            self.sel = (self.view.0 + span * 0.3, self.view.1 - span * 0.3);
            self.sel_init = true;
        }
        self.scope = scope;
    }
}
