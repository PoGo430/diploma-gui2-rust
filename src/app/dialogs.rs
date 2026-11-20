//! Modal windows: file picker, confirmations, errors, About.

use egui::{Id, RichText};

use super::{AnalyzerApp, Confirm};
use crate::ui::theme::{ACCENT, BTN_AMBER, BTN_RED, MUTED, RED};
use crate::ui::widgets::button;

const REPO_URL: &str = "https://github.com/PoGo430/diploma-gui2-rust";

/// (action, how)
const CONTROLS: &[(&str, &str)] = &[
    ("Zoom", "mouse wheel (around the pointer)"),
    ("Pan", "drag"),
    ("Fit all data", "double-click a plot, or Ctrl+F"),
    ("Selection", "Shift+drag; drag its edges or middle; Ctrl+E"),
    ("Add marker", "M (at the mouse, or 'now' while recording), or right-click"),
    ("Move / delete marker", "drag its line / right-click it"),
    ("Jump around", "click or drag in the overview strip"),
    ("Record / stop", "Ctrl+R"),
    ("Open / export CSV", "Ctrl+O / Ctrl+S, or drop a file on the window"),
    ("This help", "F1"),
];

impl AnalyzerApp {
    pub(super) fn dialogs(&mut self, ctx: &egui::Context) {
        self.file_dialog.update(ctx);
        if let Some(p) = self.file_dialog.take_picked() {
            self.file_picked(p);
        } else if self.file_action.is_some() && *self.file_dialog.state() != egui_file_dialog::DialogState::Open {
            self.file_action = None;
        }
        self.confirm_dialog(ctx);
        self.error_dialog(ctx);
        if self.show_about {
            self.about_dialog(ctx);
        }
    }

    fn confirm_dialog(&mut self, ctx: &egui::Context) {
        let Some(which) = self.confirm else { return };
        let (title, text, ok, fill) = match which {
            Confirm::Erase => (
                "Erase flash",
                "Erase all samples stored in the board's flash memory?\nThis cannot be undone.",
                "Erase",
                BTN_RED,
            ),
            Confirm::DumpReplace => {
                ("Dump flash", "Dumping replaces the data currently shown.\nContinue?", "Dump", BTN_AMBER)
            }
        };
        let mut answer = None;
        let m = egui::Modal::new(Id::new("confirm")).show(ctx, |ui| {
            ui.set_width(360.0);
            ui.heading(title);
            ui.add_space(6.0);
            ui.label(text);
            ui.add_space(12.0);
            ui.horizontal(|ui| {
                if button(ui, true, ok, Some(fill), "", 80.0) {
                    answer = Some(true);
                }
                if button(ui, true, "Cancel", None, "", 80.0) {
                    answer = Some(false);
                }
            });
        });
        if m.should_close() && answer.is_none() {
            answer = Some(false);
        }
        if let Some(yes) = answer {
            self.confirm = None;
            if yes {
                match which {
                    Confirm::Erase => self.erase_flash(),
                    Confirm::DumpReplace => self.start_dump(),
                }
            }
        }
    }

    fn error_dialog(&mut self, ctx: &egui::Context) {
        let Some((title, text)) = self.error_popup.clone() else { return };
        let mut close = false;
        let m = egui::Modal::new(Id::new("error")).show(ctx, |ui| {
            ui.set_max_width(480.0);
            ui.heading(RichText::new(title).color(RED));
            ui.add_space(6.0);
            ui.label(text);
            ui.add_space(12.0);
            close = button(ui, true, "OK", None, "", 80.0);
        });
        if close || m.should_close() {
            self.error_popup = None;
        }
    }

    fn about_dialog(&mut self, ctx: &egui::Context) {
        let mut close = false;
        let m = egui::Modal::new(Id::new("about")).show(ctx, |ui| {
            ui.set_width(440.0);
            ui.heading(format!("Analizator {}", env!("CARGO_PKG_VERSION")));
            ui.label(
                RichText::new("Current & power analyzer for the Analizator board and the Nordic PPK2").color(MUTED),
            );
            ui.add_space(10.0);
            egui::Grid::new("controls").num_columns(2).spacing([16.0, 4.0]).show(ui, |ui| {
                for (what, how) in CONTROLS {
                    ui.label(RichText::new(*what).strong());
                    ui.label(RichText::new(*how).color(MUTED));
                    ui.end_row();
                }
            });
            ui.add_space(10.0);
            ui.horizontal(|ui| {
                ui.label(RichText::new("Captures:").color(MUTED));
                if ui.link(self.logs_dir.display().to_string()).clicked() {
                    super::files::open_folder(&self.logs_dir);
                }
            });
            ui.horizontal(|ui| {
                ui.label(RichText::new("Source & downloads:").color(MUTED));
                ui.hyperlink_to(RichText::new(REPO_URL).color(ACCENT), REPO_URL);
            });
            ui.add_space(12.0);
            close = button(ui, true, "Close", None, "", 80.0);
        });
        if close || m.should_close() {
            self.show_about = false;
        }
    }
}
