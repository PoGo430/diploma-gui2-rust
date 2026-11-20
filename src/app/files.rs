//! Opening, saving and exporting captures; where they are stored.

use std::path::{Path, PathBuf};

use egui::{Align2, Color32, FontId, Id, LayerId, Order};

use super::{AnalyzerApp, FileAction, Job, Marker, Scope};
use crate::csvio;
use crate::data::MAX_SAMPLES;
use crate::fmt;
use crate::ui::theme::{ACCENT, GREEN};

impl AnalyzerApp {
    /// Save samples [a, b) in the background. `with_filtered` adds a filtered-current column.
    pub(super) fn save_async(&mut self, path: PathBuf, a: usize, b: usize, with_filtered: bool) {
        let x = self.store.x_range(a, b);
        let i = self.store.raw.v[a..b].to_vec();
        let f = (with_filtered && self.store.filter_active()).then(|| self.store.filt.v[a..b].to_vec());
        let meta = self.file_meta(a, b);
        self.busy = Some(format!("Saving {} samples…", fmt::count(b - a)));
        let tx = self.jobs_tx.clone();
        std::thread::spawn(move || {
            let r = match csvio::save(&path, &x, &i, f.as_deref(), &meta) {
                Ok(n) => Job::Saved(path, n),
                Err(e) => Job::Failed(e),
            };
            let _ = tx.send(r);
        });
    }

    /// Settings + markers stored in the header of a file holding samples [a, b).
    pub(super) fn file_meta(&self, a: usize, b: usize) -> csvio::Meta {
        let (x0, x1) = (self.store.x_at(a), self.store.x_at(b - 1));
        let markers = self.markers.iter().filter(|m| m.x >= x0 && m.x <= x1).map(|m| (m.x, m.label.clone())).collect();
        csvio::Meta { voltage: self.voltage, period_ms: self.period_ms, markers }
    }

    pub(super) fn open_dialog(&mut self, action: FileAction) {
        if self.busy.is_some() || self.recording() {
            return;
        }
        if matches!(action, FileAction::Export) && self.store.is_empty() {
            return;
        }
        let dir = self.s.last_dir.clone().filter(|d| d.is_dir()).unwrap_or_else(|| self.logs_dir.clone());
        let cfg = self.file_dialog.config_mut();
        cfg.initial_directory = dir;
        match action {
            FileAction::Open => {
                cfg.title = Some("Open capture".into());
                self.file_dialog.pick_file();
            }
            FileAction::Export => {
                let what = if self.scope == Scope::Selection { "selection" } else { "capture" };
                cfg.title = Some(format!("Export {what}"));
                cfg.default_file_name = capture_name(what);
                self.file_dialog.save_file();
            }
        }
        self.file_action = Some(action);
    }

    pub(super) fn file_picked(&mut self, path: PathBuf) {
        if let Some(dir) = path.parent() {
            self.s.last_dir = Some(dir.to_path_buf());
        }
        match self.file_action.take() {
            Some(FileAction::Open) => self.open_file(path),
            Some(FileAction::Export) => {
                let (a, b) = if self.scope == Scope::Selection {
                    self.store.index_range(self.sel.0, self.sel.1)
                } else {
                    (0, self.store.len())
                };
                if b <= a {
                    self.show_error("Export", "Nothing to export in the selected range.");
                    return;
                }
                let path = if path.extension().is_none() { path.with_extension("csv") } else { path };
                self.save_async(path, a, b, true);
            }
            None => {}
        }
    }

    /// Load a capture file in the background.
    pub(super) fn open_file(&mut self, path: PathBuf) {
        if self.busy.is_some() || self.recording() {
            return;
        }
        self.busy = Some(format!("Loading {}…", file_name(&path)));
        let tx = self.jobs_tx.clone();
        std::thread::spawn(move || {
            let r = match csvio::load(&path) {
                Ok(l) => Job::Loaded(path, l),
                Err(e) => Job::Failed(e),
            };
            let _ = tx.send(r);
        });
    }

    pub(super) fn on_job(&mut self, job: Job, ctx: &egui::Context) {
        self.busy = None;
        match job {
            Job::Saved(path, n) => {
                self.status = format!("Saved {} samples to {}", fmt::count(n), path.display());
                self.log(format!("Saved {} samples → {}", fmt::count(n), path.display()), GREEN);
            }
            Job::Loaded(path, l) => {
                // the file's own period and voltage; the user's settings stay as they are
                if let Some(v) = l.voltage.filter(|v| v.is_finite() && *v >= 0.0) {
                    self.voltage = v;
                }
                if let Some(p) = l.period_ms.filter(|p| p.is_finite() && *p > 0.0) {
                    self.period_ms = p;
                }
                let total = l.i.len();
                self.store.load(l.x, l.i);
                self.markers = l.markers.into_iter().map(|(x, label)| Marker { x, label }).collect();
                self.marker_seq = self.markers.len();
                self.sel_init = false;
                self.fit();
                let name = file_name(&path);
                let mut msg = format!("Loaded {} samples from {name}", fmt::count(self.store.len()));
                if total > MAX_SAMPLES {
                    msg.push_str(&format!(" (truncated to {})", fmt::count(MAX_SAMPLES)));
                }
                self.log(msg.clone(), GREEN);
                self.status = msg;
                self.title = name;
            }
            Job::Failed(e) => {
                self.status = format!("Error: {e}");
                self.show_error("Error", e);
            }
        }
        if self.quit_after_job {
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }
    }

    /// Open a CSV file dropped onto the window.
    pub(super) fn handle_dropped_files(&mut self, ctx: &egui::Context) {
        let dropped = ctx.input(|i| {
            i.raw
                .dropped_files
                .iter()
                .map(|f| f.path().to_path_buf())
                .filter(|p| !p.as_os_str().is_empty())
                .collect::<Vec<_>>()
        });
        if let Some(path) = dropped.into_iter().next() {
            if self.recording() {
                self.show_error("Open", "Stop the recording before opening a file.");
            } else {
                self.open_file(path);
            }
        }
    }

    /// "Drop to open" hint while a file is dragged over the window.
    pub(super) fn drop_overlay(&self, ctx: &egui::Context) {
        if ctx.input(|i| i.raw.hovered_files.is_empty()) {
            return;
        }
        let painter = ctx.layer_painter(LayerId::new(Order::Foreground, Id::new("drop")));
        let rect = ctx.content_rect();
        painter.rect_filled(rect, 0.0, Color32::from_black_alpha(170));
        painter.rect_stroke(rect.shrink(12.0), 12.0, egui::Stroke::new(2.0, ACCENT), egui::StrokeKind::Inside);
        painter.text(
            rect.center(),
            Align2::CENTER_CENTER,
            "Drop a CSV file to open it",
            FontId::proportional(22.0),
            Color32::WHITE,
        );
    }
}

/// "power_capture_log_20261120_121759.csv"
pub fn capture_name(prefix: &str) -> String {
    format!("{prefix}_{}.csv", chrono::Local::now().format("%Y%m%d_%H%M%S"))
}

pub fn file_name(p: &Path) -> String {
    p.file_name().map(|s| s.to_string_lossy().to_string()).unwrap_or_else(|| p.display().to_string())
}

/// `logs` next to the program (e.g. on a USB stick) if that folder is visible and writable,
/// otherwise ~/Analizator/logs (e.g. when installed into ~/.local or Program Files).
pub fn logs_dir() -> PathBuf {
    let exe_dir = std::env::current_exe().ok().and_then(|p| p.parent().map(|d| d.to_path_buf()));
    let usable = exe_dir.as_ref().is_some_and(|d| {
        let s = d.to_string_lossy();
        !s.contains("/.")
            && !s.starts_with("/usr")
            && !s.starts_with("/opt")
            && !s.to_lowercase().contains("\\program files")
    });
    if let Some(dir) = exe_dir.filter(|_| usable).map(|d| d.join("logs"))
        && std::fs::create_dir_all(&dir).is_ok()
    {
        let probe = dir.join(".write_test");
        if std::fs::write(&probe, b"").is_ok() {
            let _ = std::fs::remove_file(probe);
            return dir;
        }
    }
    let home = std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."));
    home.join("Analizator").join("logs")
}

/// Show a folder in the system file manager.
pub fn open_folder(dir: &Path) {
    let _ = std::fs::create_dir_all(dir);
    let cmd = if cfg!(target_os = "windows") {
        "explorer"
    } else if cfg!(target_os = "macos") {
        "open"
    } else {
        "xdg-open"
    };
    let _ = std::process::Command::new(cmd).arg(dir).spawn();
}
