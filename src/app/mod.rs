//! The main window.
//!
//! * `mod.rs`        state, settings, the per-frame loop
//! * `connection.rs` device list, connect/reconnect, recording
//! * `files.rs`      CSV open/save/export, logs folder
//! * `topbar.rs`     toolbar and status bar
//! * `sidebar.rs`    settings, filter, statistics, battery, markers, device log
//! * `plots.rs`      current/power plots and the overview strip
//! * `markers.rs`    marker list helpers
//! * `dialogs.rs`    confirmations, errors, About

mod connection;
mod dialogs;
pub mod files;
mod markers;
mod plots;
mod sidebar;
mod topbar;

use std::collections::{BTreeMap, VecDeque};
use std::path::PathBuf;
use std::sync::mpsc::{Receiver, Sender, channel};
use std::time::{Duration, Instant};

use egui::{Align2, Color32, Frame, Margin, Stroke, Ui};
use egui_file_dialog::FileDialog;
use egui_plot::PlotTransform;
use serde::{Deserialize, Serialize};

use crate::csvio;
use crate::data::DataStore;
use crate::device::{self, Device, DeviceEntry, Level};
use crate::filter::FilterCfg;
use crate::ui::theme::{self, AMBER, BG, BORDER, GREEN, MUTED, PANEL, RED, TEXT};

pub use markers::Marker;

const LOG_LINES: usize = 5000;

/// Remembered between runs.
#[derive(Serialize, Deserialize, Clone)]
#[serde(default)]
pub struct Settings {
    /// Supply voltage of the device under test, for devices that do not set it themselves.
    dut_voltage: f64,
    /// Sample period of devices that do not fix it themselves (the board).
    sample_period_ms: f64,
    baud: u32,
    /// Id of the last device used (port path, "__demo__", …).
    #[serde(alias = "last_port")]
    last_device: String,
    x_time: bool,
    live_window_s: f64,
    follow: bool,
    show_power: bool,
    show_overview: bool,
    filter: FilterCfg,
    show_raw: bool,
    stats_filtered: bool,
    battery_mah: f64,
    threshold_ma: f64,
    show_threshold: bool,
    /// Per-device preferences set by the devices themselves (e.g. "ppk2.decimation").
    device_prefs: BTreeMap<String, String>,
    /// Folder of the last opened/exported file.
    last_dir: Option<PathBuf>,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            dut_voltage: 3.3,
            sample_period_ms: 1.0,
            baud: 921_600,
            last_device: String::new(),
            x_time: true,
            live_window_s: 10.0,
            follow: true,
            show_power: true,
            show_overview: true,
            filter: FilterCfg::default(),
            show_raw: true,
            stats_filtered: false,
            battery_mah: 220.0,
            threshold_ma: 1.0,
            show_threshold: true,
            device_prefs: BTreeMap::new(),
            last_dir: None,
        }
    }
}

#[derive(PartialEq, Clone, Copy, Debug)]
enum Mode {
    Offline,
    /// Opening a device on a background thread.
    Connecting,
    Connected,
    Capture,
    Dump,
    /// The device disappeared (e.g. USB unplugged); trying to open it again every second.
    Reconnecting,
}

#[derive(PartialEq, Clone, Copy)]
enum Scope {
    All,
    Selection,
    Visible,
}

impl Scope {
    fn name(self) -> &'static str {
        match self {
            Scope::All => "whole capture",
            Scope::Selection => "selection",
            Scope::Visible => "visible window",
        }
    }
}

/// What the mouse is dragging in a plot.
#[derive(PartialEq, Clone, Copy)]
enum Grab {
    None,
    Left,
    Right,
    Body(f64),
    New(f64),
    Marker(usize),
}

#[derive(PartialEq, Clone, Copy)]
enum Confirm {
    Erase,
    DumpReplace,
}

#[derive(Clone, Copy)]
enum FileAction {
    Open,
    Export,
}

enum Job {
    Saved(PathBuf, usize),
    Loaded(PathBuf, csvio::Loaded),
    Failed(String),
}

struct LogLine {
    time: String,
    text: String,
    color: Color32,
}

/// (time in samples, sum of current) below and above the sleep/active threshold.
type StateSplit = ((usize, f64), (usize, f64));

/// Result of opening a device on a background thread.
type Opened = (DeviceEntry, Result<Box<dyn Device>, String>);

/// Key of the cached sleep/active split: (data generation, a, b, threshold bits, filtered).
type SplitKey = (u64, usize, usize, u64, bool);

/// A device being opened on a background thread.
struct Opening {
    rx: Receiver<Opened>,
    reconnect: bool,
}

/// A device that disappeared and is being waited for.
struct Lost {
    entry: DeviceEntry,
    state: Option<String>,
    last_try: Instant,
}

pub struct AnalyzerApp {
    s: Settings,
    store: DataStore,
    /// Sample period of the data shown (ms): from the device that recorded it or the file.
    period_ms: f64,
    /// Supply voltage used for the power of the data shown (V).
    voltage: f64,
    dev: Option<Box<dyn Device>>,
    devices: Vec<DeviceEntry>,
    device_sel: String,
    baud_text: String,
    mode: Mode,
    opening: Option<Opening>,
    lost: Option<Lost>,
    record_start: f64,
    capture_t0: Instant,
    dump_t0: Instant,
    last_data: Option<Instant>,
    full_warned: bool,

    /// Visible range in sample-index units.
    view: (f64, f64),
    scope: Scope,
    sel: (f64, f64),
    sel_init: bool,
    grab: Grab,
    grab_plot: usize,
    last_tf: [Option<PlotTransform>; 2],
    hover_x: Option<f64>,
    /// Where the plot's right-click menu was opened (sample index) and the marker under it.
    ctx_menu: Option<(f64, Option<usize>)>,
    markers: Vec<Marker>,
    marker_seq: usize,
    focus_marker: Option<usize>,
    focus_tries: u32,

    log: VecDeque<LogLine>,
    status: String,
    busy: Option<String>,
    jobs_tx: Sender<Job>,
    jobs_rx: Receiver<Job>,
    quit_after_job: bool,
    file_dialog: FileDialog,
    file_action: Option<FileAction>,
    confirm: Option<Confirm>,
    error_popup: Option<(String, String)>,
    show_about: bool,

    logs_dir: PathBuf,
    rate_count: usize,
    rate_t0: Instant,
    rate: f64,
    last_scan: Instant,
    title: String,
    decim: Vec<(f64, f64)>,
    decim_raw: Vec<(f64, f64)>,
    /// Filter settings being edited (applied when a drag ends, to avoid recomputing every frame).
    filter_edit: FilterCfg,
    state_cache: Option<(SplitKey, Instant, StateSplit)>,
}

impl AnalyzerApp {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        theme::apply(&cc.egui_ctx);
        let s: Settings = cc.storage.and_then(|st| eframe::get_value(st, eframe::APP_KEY)).unwrap_or_default();
        let (jobs_tx, jobs_rx) = channel();
        let devices = device::discover();
        let device_sel = if devices.iter().any(|e| e.id == s.last_device) {
            s.last_device.clone()
        } else {
            devices.first().map(|e| e.id.clone()).unwrap_or_default()
        };
        let file_dialog = FileDialog::new()
            .add_file_filter_extensions("CSV files", vec!["csv"])
            .default_file_filter("CSV files")
            .add_save_extension("CSV file", "csv")
            .default_save_extension("CSV file")
            .default_size([760.0, 480.0])
            .anchor(Align2::CENTER_CENTER, [0.0, 0.0]);
        let mut app = Self {
            baud_text: s.baud.to_string(),
            period_ms: s.sample_period_ms,
            voltage: s.dut_voltage,
            s,
            store: DataStore::default(),
            dev: None,
            devices,
            device_sel,
            mode: Mode::Offline,
            opening: None,
            lost: None,
            record_start: 0.0,
            capture_t0: Instant::now(),
            dump_t0: Instant::now(),
            last_data: None,
            full_warned: false,
            view: (0.0, 10_000.0),
            scope: Scope::All,
            sel: (0.0, 0.0),
            sel_init: false,
            grab: Grab::None,
            grab_plot: 0,
            last_tf: [None, None],
            hover_x: None,
            ctx_menu: None,
            markers: Vec::new(),
            marker_seq: 0,
            focus_marker: None,
            focus_tries: 0,
            log: VecDeque::new(),
            status: "Ready. Choose a device and press Connect (or pick DEMO to try it out).".into(),
            busy: None,
            jobs_tx,
            jobs_rx,
            quit_after_job: false,
            file_dialog,
            file_action: None,
            confirm: None,
            error_popup: None,
            show_about: false,
            logs_dir: files::logs_dir(),
            rate_count: 0,
            rate_t0: Instant::now(),
            rate: 0.0,
            last_scan: Instant::now(),
            title: String::new(),
            decim: Vec::new(),
            decim_raw: Vec::new(),
            filter_edit: FilterCfg::default(),
            state_cache: None,
        };
        app.s.filter = app.s.filter.sanitized();
        app.filter_edit = app.s.filter;
        app.store.set_filter(app.s.filter, app.dt());
        app.view = (0.0, app.s.live_window_s / app.dt());
        let d = app.logs_dir.display().to_string();
        app.log(format!("Analizator {} · captures are saved to {d}", env!("CARGO_PKG_VERSION")), MUTED);
        if let Some(path) = std::env::args_os().nth(1).map(PathBuf::from).filter(|p| p.is_file()) {
            app.open_file(path);
        }
        app
    }

    // ---------------------------------------------------------- helpers ---
    fn dt(&self) -> f64 {
        (self.period_ms / 1000.0).max(1e-9)
    }

    /// Multiplier from sample index to plot x units (seconds or samples).
    fn xscale(&self) -> f64 {
        if self.s.x_time { self.dt() } else { 1.0 }
    }

    fn recording(&self) -> bool {
        matches!(self.mode, Mode::Capture | Mode::Dump)
    }

    fn connected(&self) -> bool {
        self.dev.is_some()
    }

    fn log(&mut self, text: impl Into<String>, color: Color32) {
        let time = chrono::Local::now().format("%H:%M:%S").to_string();
        self.log.push_back(LogLine { time, text: text.into(), color });
        while self.log.len() > LOG_LINES {
            self.log.pop_front();
        }
    }

    fn log_level(&mut self, level: Level, text: impl Into<String>) {
        let color = match level {
            Level::Info => TEXT,
            Level::Good => GREEN,
            Level::Warn => AMBER,
            Level::Error => RED,
        };
        self.log(text, color);
    }

    fn show_error(&mut self, title: &str, text: impl Into<String>) {
        let text = text.into();
        self.log(text.clone(), RED);
        self.error_popup = Some((title.to_string(), text));
    }

    /// Everything that happens every frame before drawing.
    fn poll(&mut self, ctx: &egui::Context) {
        self.store.set_filter(self.s.filter, self.dt());
        self.poll_device(ctx);
        while let Ok(j) = self.jobs_rx.try_recv() {
            self.on_job(j, ctx);
        }
        let el = self.rate_t0.elapsed().as_secs_f64();
        if el >= 1.0 {
            self.rate = self.rate_count as f64 / el;
            self.rate_count = 0;
            self.rate_t0 = Instant::now();
        }
        if self.recording() && self.s.follow && !self.store.is_empty() {
            let span = (self.s.live_window_s / self.dt()).max(10.0);
            let end = self.store.last_x();
            self.view = ((end - span).max(self.store.first_x()), end.max(self.store.first_x() + 10.0));
        }
        self.handle_dropped_files(ctx);
        if ctx.input(|i| i.viewport().close_requested()) {
            self.on_close_requested(ctx);
        }
    }

    fn on_close_requested(&mut self, ctx: &egui::Context) {
        if self.recording() {
            // save synchronously so nothing is lost
            let _ = self.dev.as_mut().map(|d| d.stop());
            let (a, b) = self.store.index_range(self.record_start, f64::INFINITY);
            if b > a {
                let path = self.logs_dir.join(files::capture_name("power_capture_log"));
                let meta = self.file_meta(a, b);
                let _ = csvio::save(&path, &self.store.x_range(a, b), &self.store.raw.v[a..b], None, &meta);
            }
            self.mode = Mode::Connected;
        }
        if self.busy.is_some() {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            self.quit_after_job = true;
            return;
        }
        if let Some(d) = self.dev.take() {
            d.close();
        }
    }
}

impl eframe::App for AnalyzerApp {
    fn ui(&mut self, ui: &mut Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        self.poll(&ctx);
        self.shortcuts(&ctx);

        let title = if self.title.is_empty() {
            "Analizator — Current & Power Analyzer".to_string()
        } else {
            format!("Analizator — {}", self.title)
        };
        ctx.send_viewport_cmd(egui::ViewportCommand::Title(title));

        egui::Panel::top("top")
            .frame(Frame::new().fill(PANEL).inner_margin(Margin::symmetric(12, 8)).stroke(Stroke::new(1.0, BORDER)))
            .exact_size(48.0)
            .show(ui, |ui| self.top_bar(ui));
        egui::Panel::bottom("status")
            .frame(Frame::new().fill(PANEL).inner_margin(Margin::symmetric(10, 3)).stroke(Stroke::new(1.0, BORDER)))
            .exact_size(26.0)
            .show(ui, |ui| self.status_bar(ui));
        egui::Panel::right("side")
            .frame(Frame::new().fill(BG).inner_margin(Margin { left: 4, right: 10, top: 0, bottom: 8 }))
            .resizable(true)
            .default_size(340.0)
            .size_range(300.0..=460.0)
            .show(ui, |ui| self.sidebar(ui));
        egui::CentralPanel::no_frame()
            .frame(Frame::new().fill(BG).inner_margin(Margin { left: 10, right: 6, top: 8, bottom: 8 }))
            .show(ui, |ui| self.plots(ui));

        self.dialogs(&ctx);
        self.drop_overlay(&ctx);

        // New data wakes the window up by itself (see device::link); these are
        // only for clocks and timeouts.
        let wake = if self.recording() || self.busy.is_some() || self.mode == Mode::Connecting {
            Duration::from_millis(100)
        } else {
            Duration::from_millis(1000)
        };
        ctx.request_repaint_after(wake);
    }

    fn save(&mut self, storage: &mut dyn eframe::Storage) {
        eframe::set_value(storage, eframe::APP_KEY, &self.s);
    }
}
