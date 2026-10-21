//! The main window.

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::time::{Duration, Instant};

use egui::{
    Align, Align2, Color32, CornerRadius, CursorIcon, Frame, Id, Key, KeyboardShortcut, Layout, Margin, Modifiers,
    RichText, Stroke, Ui, vec2,
};
use egui_file_dialog::FileDialog;
use egui_plot::{HLine, Line, Plot, PlotBounds, PlotPoint, PlotPoints, PlotTransform, Span, Text, VLine};
use serde::{Deserialize, Serialize};

use crate::csvio;
use crate::data::{DataStore, MAX_SAMPLES};
use crate::filter::{FilterCfg, FilterKind, MAX_AVG_WINDOW, MAX_MEDIAN_WINDOW};
use crate::fmt;
use crate::serial::{self, Connection, DEMO_PORT, Msg, PortInfo};

// ------------------------------------------------------------- colours ---
const BG: Color32 = Color32::from_rgb(0x0d, 0x11, 0x17);
const PANEL: Color32 = Color32::from_rgb(0x16, 0x1b, 0x22);
const PANEL_2: Color32 = Color32::from_rgb(0x21, 0x26, 0x2d);
const BORDER: Color32 = Color32::from_rgb(0x30, 0x36, 0x3d);
const GRID: Color32 = Color32::from_rgb(0x5a, 0x63, 0x6e);
const TEXT: Color32 = Color32::from_rgb(0xc9, 0xd1, 0xd9);
const MUTED: Color32 = Color32::from_rgb(0x8b, 0x94, 0x9e);
const ACCENT: Color32 = Color32::from_rgb(0x58, 0xa6, 0xff);
const GREEN: Color32 = Color32::from_rgb(0x3f, 0xb9, 0x50);
const RED: Color32 = Color32::from_rgb(0xf8, 0x51, 0x49);
const AMBER: Color32 = Color32::from_rgb(0xd2, 0x99, 0x22);
const CURRENT_C: Color32 = Color32::from_rgb(0xf1, 0xe0, 0x5a);
const POWER_C: Color32 = Color32::from_rgb(0x58, 0xa6, 0xff);
const BTN_GREEN: Color32 = Color32::from_rgb(0x23, 0x86, 0x36);
const BTN_RED: Color32 = Color32::from_rgb(0xb6, 0x23, 0x24);
const BTN_AMBER: Color32 = Color32::from_rgb(0x9e, 0x6a, 0x03);
const BTN_BLUE: Color32 = Color32::from_rgb(0x1f, 0x6f, 0xeb);
const MARKER_C: Color32 = Color32::from_rgb(0xd2, 0xa8, 0xff);

const BAUD_RATES: [u32; 7] = [115_200, 230_400, 460_800, 921_600, 1_000_000, 2_000_000, 3_000_000];
const DUMP_IDLE_TIMEOUT: Duration = Duration::from_millis(1500);
const DUMP_START_TIMEOUT: Duration = Duration::from_secs(15);
const LOG_LINES: usize = 5000;

/// Remembered between runs.
#[derive(Serialize, Deserialize, Clone)]
#[serde(default)]
pub struct Settings {
    voltage: f64,
    period_ms: f64,
    baud: u32,
    last_port: String,
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
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            voltage: 3.3,
            period_ms: 1.0,
            baud: 921_600,
            last_port: String::new(),
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
        }
    }
}

#[derive(PartialEq, Clone, Copy)]
enum Mode {
    Offline,
    Connected,
    Capture,
    Dump,
    /// The device disappeared (e.g. USB unplugged); trying to reopen it every second.
    Reconnecting,
}

struct Reconnect {
    path: String,
    desc: String,
    baud: u32,
    last_try: Instant,
}

#[derive(Clone)]
struct Marker {
    /// Sample index.
    x: f64,
    label: String,
}

/// (time in samples, sum of current) below and above the sleep/active threshold.
type StateSplit = ((usize, f64), (usize, f64));

#[derive(PartialEq, Clone, Copy)]
enum Scope {
    All,
    Selection,
    Visible,
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

pub struct AnalyzerApp {
    s: Settings,
    store: DataStore,
    conn: Option<Connection>,
    ports: Vec<PortInfo>,
    port_sel: String,
    baud_text: String,
    mode: Mode,
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

    logs_dir: PathBuf,
    rate_count: usize,
    rate_t0: Instant,
    rate: f64,
    last_port_scan: Instant,
    title: String,
    decim: Vec<(f64, f64)>,
    decim_raw: Vec<(f64, f64)>,
    /// Filter settings being edited (applied when a drag ends, to avoid recomputing every frame).
    filter_edit: FilterCfg,
    reconnect: Option<Reconnect>,
    markers: Vec<Marker>,
    marker_seq: usize,
    focus_marker: Option<usize>,
    focus_tries: u32,
    /// Where the plot's right-click menu was opened (sample index) and the marker under it.
    ctx_menu: Option<(f64, Option<usize>)>,
    state_cache: Option<((u64, usize, usize, u64, bool), Instant, StateSplit)>,
}

impl AnalyzerApp {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        apply_theme(&cc.egui_ctx);
        let s: Settings = cc.storage.and_then(|st| eframe::get_value(st, eframe::APP_KEY)).unwrap_or_default();
        let (jobs_tx, jobs_rx) = channel();
        let logs_dir = pick_logs_dir();
        let ports = serial::list_ports();
        let port_sel = if ports.iter().any(|p| p.path == s.last_port) || s.last_port == DEMO_PORT {
            s.last_port.clone()
        } else {
            ports.first().map(|p| p.path.clone()).unwrap_or_else(|| DEMO_PORT.to_string())
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
            s,
            store: DataStore::default(),
            conn: None,
            ports,
            port_sel,
            mode: Mode::Offline,
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
            log: VecDeque::new(),
            status: "Ready. Choose a port and press Connect (or pick DEMO to try it out).".into(),
            busy: None,
            jobs_tx,
            jobs_rx,
            quit_after_job: false,
            file_dialog,
            file_action: None,
            confirm: None,
            error_popup: None,
            logs_dir,
            rate_count: 0,
            rate_t0: Instant::now(),
            rate: 0.0,
            last_port_scan: Instant::now(),
            title: String::new(),
            decim: Vec::new(),
            decim_raw: Vec::new(),
            filter_edit: FilterCfg::default(),
            reconnect: None,
            markers: Vec::new(),
            marker_seq: 0,
            focus_marker: None,
            focus_tries: 0,
            ctx_menu: None,
            state_cache: None,
        };
        app.s.filter = app.s.filter.sanitized();
        app.filter_edit = app.s.filter;
        app.store.set_filter(app.s.filter, app.dt());
        app.view = (0.0, app.s.live_window_s / app.dt());
        let d = app.logs_dir.display().to_string();
        app.log(format!("Captures are saved to {d}"), MUTED);
        if let Some(path) = std::env::args_os().nth(1).map(PathBuf::from).filter(|p| p.is_file()) {
            app.file_action = Some(FileAction::Open);
            app.file_picked(path);
        }
        app
    }

    // ---------------------------------------------------------- helpers ---
    fn dt(&self) -> f64 {
        (self.s.period_ms / 1000.0).max(1e-9)
    }

    /// Multiplier from sample index to plot x units (seconds or samples).
    fn xscale(&self) -> f64 {
        if self.s.x_time { self.dt() } else { 1.0 }
    }

    fn recording(&self) -> bool {
        matches!(self.mode, Mode::Capture | Mode::Dump)
    }

    fn log(&mut self, text: impl Into<String>, color: Color32) {
        let time = chrono::Local::now().format("%H:%M:%S").to_string();
        self.log.push_back(LogLine { time, text: text.into(), color });
        while self.log.len() > LOG_LINES {
            self.log.pop_front();
        }
    }

    fn show_error(&mut self, title: &str, text: impl Into<String>) {
        let text = text.into();
        self.log(text.clone(), RED);
        self.error_popup = Some((title.to_string(), text));
    }

    // ------------------------------------------------------- connection ---
    fn toggle_connection(&mut self, ctx: &egui::Context) {
        if self.mode == Mode::Reconnecting {
            self.reconnect = None;
            self.mode = Mode::Offline;
            self.log("Stopped waiting for the device.", MUTED);
            self.status = "Disconnected".into();
            return;
        }
        if self.conn.is_some() {
            self.disconnect(None);
            return;
        }
        let baud = match self.baud_text.trim().parse::<u32>() {
            Ok(b) if b >= 300 => b,
            _ => {
                self.show_error("Invalid baud rate", format!("'{}' is not a valid baud rate.", self.baud_text));
                return;
            }
        };
        self.s.baud = baud;
        match Connection::open(&self.port_sel, baud, self.dt(), ctx.clone()) {
            Ok(c) => {
                let name = c.name.clone();
                self.conn = Some(c);
                self.mode = Mode::Connected;
                self.s.last_port = self.port_sel.clone();
                self.log(format!("Connected to {name}"), GREEN);
                self.status = format!("Connected to {name}");
            }
            Err(e) => self.show_error("Connection error", e),
        }
    }

    fn disconnect(&mut self, reason: Option<String>) {
        match self.mode {
            Mode::Capture => self.finish_capture(),
            Mode::Dump => self.finish_dump(),
            _ => {}
        }
        if let Some(c) = self.conn.take() {
            c.close();
        }
        self.mode = Mode::Offline;
        match reason {
            Some(r) => {
                self.status = format!("Disconnected — {r}");
                self.show_error("Connection lost", format!("The serial connection was lost:\n{r}"));
            }
            None => {
                self.log("Disconnected", MUTED);
                self.status = "Disconnected".into();
            }
        }
    }

    /// The port failed (usually: USB unplugged). Save what we have and wait for the device to return.
    fn connection_lost(&mut self, err: String) {
        let was_recording = self.recording();
        match self.mode {
            Mode::Capture => self.finish_capture(),
            Mode::Dump => self.finish_dump(),
            _ => {}
        }
        let path = self.conn.as_ref().map(|c| c.name.clone()).unwrap_or_default();
        if let Some(c) = self.conn.take() {
            c.close();
        }
        let desc = self.ports.iter().find(|p| p.path == path).map(|p| p.desc.clone()).unwrap_or_default();
        self.log(format!("Connection to {path} lost ({err}). Waiting for the device to come back…"), AMBER);
        if was_recording {
            self.log("The recording up to this point was saved.", AMBER);
        }
        self.status = format!("Connection lost — waiting for {path} to come back (press Cancel to stop)");
        self.reconnect = Some(Reconnect { path, desc, baud: self.s.baud, last_try: Instant::now() });
        self.mode = Mode::Reconnecting;
    }

    fn try_reconnect(&mut self, ctx: &egui::Context) {
        let Some(r) = self.reconnect.as_mut() else { return };
        if r.last_try.elapsed() < Duration::from_secs(1) {
            return;
        }
        r.last_try = Instant::now();
        // same device path, or (if it came back under another name) the same USB device
        let present = if cfg!(target_os = "linux") {
            Path::new(&r.path).exists()
        } else {
            serial::list_ports().iter().any(|p| p.path == r.path)
        };
        let candidate = if present {
            Some(r.path.clone())
        } else if !r.desc.is_empty() {
            serial::list_ports().into_iter().find(|p| p.desc == r.desc).map(|p| p.path)
        } else {
            None
        };
        let Some(path) = candidate else { return };
        let baud = r.baud;
        if let Ok(c) = Connection::open(&path, baud, self.dt(), ctx.clone()) {
            self.conn = Some(c);
            self.reconnect = None;
            self.mode = Mode::Connected;
            self.port_sel = path.clone();
            self.log(format!("Reconnected to {path}. Press Record to continue."), GREEN);
            self.status = format!("Reconnected to {path}");
        }
    }

    fn log_frame_counts(&mut self) {
        if let Some((f, c)) = self.conn.as_ref().map(|c| c.frame_counts()) {
            let col = if c > 0 { AMBER } else { MUTED };
            self.log(format!("{} frames received, {} corrupted", fmt::count(f as usize), fmt::count(c as usize)), col);
        }
    }

    fn send(&mut self, cmd: &str) -> bool {
        let Some(c) = self.conn.as_mut() else { return false };
        match c.send(cmd) {
            Ok(()) => {
                self.log(format!("> {cmd}"), ACCENT);
                true
            }
            Err(e) => {
                self.log(format!("Failed to send {cmd}: {e}"), RED);
                false
            }
        }
    }

    // ---------------------------------------------------------- capture ---
    fn toggle_record(&mut self) {
        if self.recording() {
            self.stop();
        } else if self.conn.is_some() && self.busy.is_none() {
            self.start_capture();
        }
    }

    fn start_capture(&mut self) {
        if !self.send("START") {
            return;
        }
        if let Some(c) = &self.conn {
            c.reset_frame_counts();
        }
        self.mode = Mode::Capture;
        self.record_start = self.store.next_index;
        self.capture_t0 = Instant::now();
        self.full_warned = false;
        self.s.follow = true;
        self.status = "Recording…".into();
    }

    fn stop(&mut self) {
        self.send("STOP");
        match self.mode {
            Mode::Capture => self.finish_capture(),
            Mode::Dump => self.finish_dump(),
            _ => {}
        }
    }

    fn after_recording_mode(&self) -> Mode {
        if self.conn.is_some() { Mode::Connected } else { Mode::Offline }
    }

    fn finish_capture(&mut self) {
        self.log_frame_counts();
        self.mode = self.after_recording_mode();
        self.fit();
        let (a, b) = self.store.index_range(self.record_start, f64::INFINITY);
        if b > a {
            let name = format!("power_capture_log_{}.csv", chrono::Local::now().format("%Y%m%d_%H%M%S"));
            let path = self.logs_dir.join(name);
            self.save_async(path, a, b, false);
        } else {
            self.status = "Capture stopped (no samples received).".into();
        }
    }

    fn request_dump(&mut self) {
        if self.store.is_empty() {
            self.start_dump();
        } else {
            self.confirm = Some(Confirm::DumpReplace);
        }
    }

    fn start_dump(&mut self) {
        self.clear_data();
        if !self.send("DUMP") {
            return;
        }
        if let Some(c) = &self.conn {
            c.reset_frame_counts();
        }
        self.mode = Mode::Dump;
        self.record_start = 0.0;
        self.dump_t0 = Instant::now();
        self.last_data = None;
        self.s.follow = true;
        self.status = "Dumping flash…".into();
    }

    fn finish_dump(&mut self) {
        self.log_frame_counts();
        self.mode = self.after_recording_mode();
        self.fit();
        let n = self.store.len();
        if n > 0 {
            self.log(format!("Flash dump finished: {} samples", fmt::count(n)), GREEN);
            let name = format!("flash_dump_{}.csv", chrono::Local::now().format("%Y%m%d_%H%M%S"));
            let path = self.logs_dir.join(name);
            self.save_async(path, 0, n, false);
        } else {
            self.log("Flash dump: no data received", AMBER);
            self.status = "Flash dump: no data received.".into();
        }
    }

    fn clear_data(&mut self) {
        if self.mode == Mode::Capture {
            self.record_start = 0.0;
        }
        self.store.clear();
        self.markers.clear();
        self.marker_seq = 0;
        self.sel_init = false;
        self.hover_x = None;
        self.view = (0.0, self.s.live_window_s / self.dt());
        self.status = "Cleared.".into();
    }

    fn fit(&mut self) {
        if self.store.is_empty() {
            return;
        }
        self.s.follow = false;
        let (x0, x1) = (self.store.first_x(), self.store.last_x());
        let pad = ((x1 - x0) * 0.01).max(1.0);
        self.view = (x0 - pad, x1 + pad);
    }

    fn on_data(&mut self, chunk: Vec<f64>) {
        if !self.recording() {
            return; // the board is chatty outside a capture; ignore
        }
        let stored = self.store.append(&chunk);
        if stored < chunk.len() && !self.full_warned {
            self.full_warned = true;
            self.log(format!("Memory limit reached ({} samples) — stopping.", fmt::count(MAX_SAMPLES)), RED);
            self.stop();
        }
        self.last_data = Some(Instant::now());
        self.rate_count += stored;
    }

    // ------------------------------------------------------------ files ---
    /// Save samples [a, b) in the background. `with_filtered` adds a filtered-current column.
    fn save_async(&mut self, path: PathBuf, a: usize, b: usize, with_filtered: bool) {
        let x = self.store.x[a..b].to_vec();
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
    fn file_meta(&self, a: usize, b: usize) -> csvio::Meta {
        let (x0, x1) = (self.store.x[a], self.store.x[b - 1]);
        let markers = self.markers.iter().filter(|m| m.x >= x0 && m.x <= x1).map(|m| (m.x, m.label.clone())).collect();
        csvio::Meta { voltage: self.s.voltage, period_ms: self.s.period_ms, markers }
    }

    /// Keep a position inside the recorded data.
    fn clamp_x(&self, x: f64) -> f64 {
        if self.store.is_empty() { x } else { x.clamp(self.store.first_x(), self.store.last_x()) }
    }

    fn add_marker(&mut self, x: f64) {
        let x = self.clamp_x(x);
        self.marker_seq += 1;
        let m = Marker { x, label: format!("Marker {}", self.marker_seq) };
        let k = self.markers.partition_point(|o| o.x <= x);
        self.markers.insert(k, m);
    }

    fn sort_markers(&mut self) {
        self.markers.sort_by(|a, b| a.x.total_cmp(&b.x));
    }

    fn open_dialog(&mut self, action: FileAction) {
        if self.busy.is_some() || self.recording() {
            return;
        }
        if matches!(action, FileAction::Export) && self.store.is_empty() {
            return;
        }
        let cfg = self.file_dialog.config_mut();
        cfg.initial_directory = self.logs_dir.clone();
        match action {
            FileAction::Open => {
                cfg.title = Some("Open capture".into());
                self.file_dialog.pick_file();
            }
            FileAction::Export => {
                let what = if self.scope == Scope::Selection { "selection" } else { "capture" };
                cfg.title = Some(format!("Export {what}"));
                cfg.default_file_name = format!("{what}_{}.csv", chrono::Local::now().format("%Y%m%d_%H%M%S"));
                self.file_dialog.save_file();
            }
        }
        self.file_action = Some(action);
    }

    fn file_picked(&mut self, path: PathBuf) {
        match self.file_action.take() {
            Some(FileAction::Open) => {
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

    fn on_job(&mut self, job: Job, ctx: &egui::Context) {
        self.busy = None;
        match job {
            Job::Saved(path, n) => {
                self.status = format!("Saved {} samples to {}", fmt::count(n), path.display());
                self.log(format!("Saved {} samples → {}", fmt::count(n), path.display()), GREEN);
            }
            Job::Loaded(path, l) => {
                if let Some(v) = l.voltage.filter(|v| v.is_finite() && *v >= 0.0) {
                    self.s.voltage = v;
                }
                if let Some(p) = l.period_ms.filter(|p| p.is_finite() && *p > 0.0) {
                    self.s.period_ms = p;
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

    // ------------------------------------------------------- per frame ---
    fn poll(&mut self, ctx: &egui::Context) {
        self.store.set_filter(self.s.filter, self.dt());
        // serial messages
        let mut msgs = Vec::new();
        if let Some(c) = &self.conn {
            while let Ok(m) = c.rx.try_recv() {
                msgs.push(m);
            }
        }
        for m in msgs {
            match m {
                Msg::Data(d) => self.on_data(d),
                Msg::Line(t) => self.log(t, TEXT),
                Msg::Error(e) => self.connection_lost(e),
            }
        }
        if self.mode == Mode::Reconnecting {
            self.try_reconnect(ctx);
        }
        // background jobs
        while let Ok(j) = self.jobs_rx.try_recv() {
            self.on_job(j, ctx);
        }
        // flash dump has no end marker: finish after a quiet period
        if self.mode == Mode::Dump {
            let done = match self.last_data {
                Some(t) => t.elapsed() > DUMP_IDLE_TIMEOUT,
                None => self.dump_t0.elapsed() > DUMP_START_TIMEOUT,
            };
            if done {
                self.finish_dump();
            }
        }
        // sample rate
        let el = self.rate_t0.elapsed().as_secs_f64();
        if el >= 1.0 {
            self.rate = self.rate_count as f64 / el;
            self.rate_count = 0;
            self.rate_t0 = Instant::now();
        }
        // watch for plugged/unplugged adapters
        if self.conn.is_none() && self.last_port_scan.elapsed() > Duration::from_millis(1500) {
            self.last_port_scan = Instant::now();
            let ports = serial::list_ports();
            if ports != self.ports {
                if self.port_sel != DEMO_PORT && !ports.iter().any(|p| p.path == self.port_sel) {
                    if let Some(p) = ports.iter().find(|p| !self.ports.contains(p)).or(ports.first()) {
                        self.port_sel = p.path.clone();
                    }
                }
                self.ports = ports;
            }
        }
        // keep following live data
        if self.recording() && self.s.follow && !self.store.is_empty() {
            let span = (self.s.live_window_s / self.dt()).max(10.0);
            let end = self.store.last_x();
            self.view = ((end - span).max(self.store.first_x()), end.max(self.store.first_x() + 10.0));
        }
        // close button while recording / saving
        if ctx.input(|i| i.viewport().close_requested()) {
            if self.recording() {
                self.send("STOP");
                let (a, b) = self.store.index_range(self.record_start, f64::INFINITY);
                if b > a {
                    let name = format!("power_capture_log_{}.csv", chrono::Local::now().format("%Y%m%d_%H%M%S"));
                    let _ = csvio::save(&self.logs_dir.join(name), &self.store.x[a..b], &self.store.raw.v[a..b], None, &self.file_meta(a, b));
                }
                self.mode = self.after_recording_mode();
            }
            if self.busy.is_some() {
                ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
                self.quit_after_job = true;
            }
        }
    }

    fn shortcuts(&mut self, ctx: &egui::Context) {
        let sc = |k| KeyboardShortcut::new(Modifiers::COMMAND, k);
        if ctx.input_mut(|i| i.consume_shortcut(&sc(Key::R))) {
            self.toggle_record();
        }
        if ctx.input_mut(|i| i.consume_shortcut(&sc(Key::O))) {
            self.open_dialog(FileAction::Open);
        }
        if ctx.input_mut(|i| i.consume_shortcut(&sc(Key::S))) {
            self.open_dialog(FileAction::Export);
        }
        if ctx.input_mut(|i| i.consume_shortcut(&sc(Key::F))) {
            self.fit();
        }
        // M = add a marker (at the mouse, or "now" while recording)
        if !ctx.egui_wants_keyboard_input() && ctx.input(|i| i.key_pressed(Key::M) && i.modifiers.is_none()) && !self.store.is_empty() {
            let x = match self.hover_x {
                Some(h) => h,
                None if self.recording() => self.store.last_x(),
                None => (self.view.0 + self.view.1) / 2.0,
            };
            self.add_marker(x);
        }
        if ctx.input_mut(|i| i.consume_shortcut(&sc(Key::E))) {
            let next = if self.scope == Scope::Selection { Scope::All } else { Scope::Selection };
            self.set_scope(next);
        }
    }

    fn set_scope(&mut self, scope: Scope) {
        if scope == Scope::Selection && !self.sel_init {
            let span = self.view.1 - self.view.0;
            self.sel = (self.view.0 + span * 0.3, self.view.1 - span * 0.3);
            self.sel_init = true;
        }
        self.scope = scope;
    }

    // --------------------------------------------------------------- UI ---
    fn top_bar(&mut self, ui: &mut Ui) {
        let ctx = ui.ctx().clone();
        let connected = self.conn.is_some();
        let linked = connected || self.mode == Mode::Reconnecting;
        let rec = self.recording();
        let busy = self.busy.is_some();
        ui.horizontal_centered(|ui| {
            ui.spacing_mut().item_spacing.x = 6.0;
            // port
            let label = if self.port_sel == DEMO_PORT {
                "DEMO  —  simulated device".to_string()
            } else {
                self.ports.iter().find(|p| p.path == self.port_sel).map(|p| p.label.clone()).unwrap_or_else(|| {
                    if self.port_sel.is_empty() { "No port".into() } else { self.port_sel.clone() }
                })
            };
            ui.add_enabled_ui(!linked, |ui| {
                egui::ComboBox::from_id_salt("port")
                    .width(250.0)
                    .selected_text(label)
                    .show_ui(ui, |ui| {
                        if self.ports.is_empty() {
                            ui.label(RichText::new("No serial ports found").color(MUTED));
                        }
                        for p in &self.ports {
                            ui.selectable_value(&mut self.port_sel, p.path.clone(), &p.label);
                        }
                        ui.separator();
                        ui.selectable_value(&mut self.port_sel, DEMO_PORT.to_string(), "DEMO  —  simulated device");
                    })
                    .response
                    .on_hover_text("Serial port");
                if ui.add(egui::Button::new("⟳").min_size(vec2(30.0, 30.0))).on_hover_text("Rescan serial ports").clicked() {
                    self.ports = serial::list_ports();
                }
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
            });
            let (t, c) = if self.mode == Mode::Reconnecting {
                ("Cancel", BTN_RED)
            } else if connected {
                ("Disconnect", BTN_RED)
            } else {
                ("Connect", BTN_GREEN)
            };
            if button(ui, true, t, Some(c), "", 104.0) {
                self.toggle_connection(&ctx);
            }
            vsep(ui);
            if button(ui, connected && !rec && !busy, "⏺  Record", Some(BTN_GREEN), "Start live capture (Ctrl+R)", 0.0) {
                self.start_capture();
            }
            if button(ui, connected && rec, "⏹  Stop", Some(BTN_RED), "Stop and save CSV (Ctrl+R)", 0.0) {
                self.stop();
            }
            if button(ui, connected && !rec && !busy, "Dump flash", Some(BTN_AMBER), "Download the samples stored in the board's flash", 0.0) {
                self.request_dump();
            }
            if button(ui, connected && !rec, "Erase flash", None, "Erase the board's flash memory", 0.0) {
                self.confirm = Some(Confirm::Erase);
            }
            vsep(ui);
            if button(ui, !rec && !busy, "Open CSV…", Some(BTN_BLUE), "Load a saved capture (Ctrl+O)", 0.0) {
                self.open_dialog(FileAction::Open);
            }
            let export_tip = "Save the whole capture, or the selection if one is active (Ctrl+S)";
            if button(ui, !rec && !busy && !self.store.is_empty(), "Export CSV…", None, export_tip, 0.0) {
                self.open_dialog(FileAction::Export);
            }
            if button(ui, !self.store.is_empty(), "Fit", None, "Zoom to show all data (Ctrl+F, or double-click a plot)", 0.0) {
                self.fit();
            }
            if button(ui, !busy, "Clear", None, "Remove all data", 0.0) {
                self.clear_data();
            }

            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                let (text, col) = if busy {
                    ("WORKING…".to_string(), ACCENT)
                } else {
                    match self.mode {
                        Mode::Capture => ("⏺  REC".to_string(), RED),
                        Mode::Dump => ("DUMPING".to_string(), AMBER),
                        Mode::Connected => (format!("CONNECTED · {}", self.conn.as_ref().map(|c| c.name.as_str()).unwrap_or("")), GREEN),
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

    fn status_bar(&self, ui: &mut Ui) {
        ui.horizontal_centered(|ui| {
            let msg = self.busy.as_deref().unwrap_or(&self.status);
            ui.label(RichText::new(msg).color(MUTED));
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                ui.label(RichText::new(fmt::si_digits(self.store.bytes() as f64, "B", 3)).color(MUTED))
                    .on_hover_text("Memory used by the samples");
                ui.add_space(12.0);
                if let Some((f, c)) = self.conn.as_ref().map(|c| c.frame_counts()) {
                    let col = if c > 0 { RED } else { MUTED };
                    ui.label(RichText::new(format!("{} corrupted", fmt::count(c as usize))).color(col))
                        .on_hover_text("Data frames that arrived damaged (missing start/end marker or a bad value) since the recording started");
                    ui.label(RichText::new(format!("{} frames ·", fmt::count(f as usize))).color(MUTED));
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

    fn sidebar(&mut self, ui: &mut Ui) {
        egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
            ui.add_space(8.0);
            section(ui, "MEASUREMENT", |ui| {
                egui::Grid::new("settings").num_columns(2).spacing([12.0, 8.0]).show(ui, |ui| {
                    ui.label(RichText::new("DUT voltage").color(MUTED));
                    ui.add(egui::DragValue::new(&mut self.s.voltage).range(0.0..=30.0).speed(0.01).fixed_decimals(3).suffix(" V"));
                    ui.end_row();
                    ui.label(RichText::new("Sample period").color(MUTED));
                    let r = ui.add(
                        egui::DragValue::new(&mut self.s.period_ms).range(0.001..=10_000.0).speed(0.01).fixed_decimals(3).suffix(" ms"),
                    );
                    if r.changed() {
                        if let Some(c) = &self.conn {
                            c.set_demo_period(self.dt());
                        }
                    }
                    r.on_hover_text("Time between samples (used for the time axis, charge and energy)");
                    ui.end_row();
                    ui.label(RichText::new("X axis").color(MUTED));
                    ui.horizontal(|ui| {
                        ui.selectable_value(&mut self.s.x_time, true, "Time");
                        ui.selectable_value(&mut self.s.x_time, false, "Sample");
                    });
                    ui.end_row();
                    ui.label(RichText::new("Live window").color(MUTED));
                    ui.add(egui::DragValue::new(&mut self.s.live_window_s).range(0.1..=36_000.0).speed(0.1).fixed_decimals(1).suffix(" s"))
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
            });

            ui.add_space(8.0);
            section(ui, "FILTER", |ui| self.filter_ui(ui));

            ui.add_space(8.0);
            section(ui, "STATISTICS", |ui| {
                ui.horizontal(|ui| {
                    ui.label(RichText::new("Range").color(MUTED));
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
                    ui.checkbox(&mut self.s.stats_filtered, "Use filtered data")
                        .on_hover_text("Off: statistics come from the raw samples (recommended for charge/energy).\nOn: from the filtered signal, e.g. for sustained peak current.");
                }
                ui.add_space(6.0);
                self.stats_tiles(ui);
                ui.add_space(6.0);
                self.cursor_readout(ui);
            });

            ui.add_space(8.0);
            section(ui, "BATTERY & SLEEP", |ui| self.analysis_ui(ui));

            ui.add_space(8.0);
            section(ui, "MARKERS", |ui| self.markers_ui(ui));

            ui.add_space(8.0);
            section(ui, "DEVICE LOG", |ui| {
                ui.horizontal(|ui| {
                    if ui.small_button("Open logs folder").on_hover_text(self.logs_dir.display().to_string()).clicked() {
                        let _ = std::fs::create_dir_all(&self.logs_dir);
                        open_folder(&self.logs_dir);
                    }
                    if ui.small_button("Clear").clicked() {
                        self.log.clear();
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
                            let fmt = |c| egui::TextFormat { font_id: font.clone(), color: c, ..Default::default() };
                            job.append(&l.time, 0.0, fmt(MUTED));
                            job.append(&l.text, 8.0, fmt(l.color));
                            ui.add(egui::Label::new(job).wrap());
                        }
                    });
            });
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
                ui.label(RichText::new("Raw data is shown. Filters only change the display; recordings are always saved unfiltered.").color(MUTED).size(12.0));
            }
            FilterKind::MovingAverage | FilterKind::Median => {
                let max = if kind == FilterKind::Median { MAX_MEDIAN_WINDOW } else { MAX_AVG_WINDOW };
                ui.horizontal(|ui| {
                    ui.label(RichText::new("Window").color(MUTED));
                    let r = ui.add(egui::DragValue::new(&mut self.filter_edit.window).range(3..=max).speed(0.5).suffix(" samples"));
                    apply |= r.drag_stopped() || (r.changed() && !r.dragged());
                    ui.label(RichText::new(format!("= {}", fmt::si((self.filter_edit.window | 1) as f64 * dt, "s"))).color(MUTED));
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
                    ui.label(RichText::new("Cut-off").color(MUTED));
                    let speed = (self.filter_edit.cutoff_hz * 0.01).max(0.001);
                    let r = ui.add(
                        egui::DragValue::new(&mut self.filter_edit.cutoff_hz).range(0.001..=nyquist).speed(speed).max_decimals(3).suffix(" Hz"),
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

    /// Sample range [a, b) the statistics are about (whole capture, selection or visible window).
    fn scope_range(&self) -> Option<(usize, usize)> {
        let n = self.store.len();
        let (a, b) = match self.scope {
            _ if n == 0 => return None,
            Scope::All => (0, n),
            Scope::Selection => self.store.index_range(self.sel.0, self.sel.1),
            Scope::Visible => self.store.index_range(self.view.0, self.view.1),
        };
        (b > a).then_some((a, b))
    }

    fn scope_agg(&self) -> Option<crate::data::Agg> {
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
            if *k == key || (self.recording() && k.3 == key.3 && k.4 == key.4 && t.elapsed() < Duration::from_millis(250)) {
                return Some(*r);
            }
        }
        let r = self.store.series(self.s.stats_filtered).split(a, b, thr);
        self.state_cache = Some((key, Instant::now(), r));
        Some(r)
    }

    fn analysis_ui(&mut self, ui: &mut Ui) {
        let scope_name = match self.scope {
            Scope::All => "whole capture",
            Scope::Selection => "selection",
            Scope::Visible => "visible window",
        };
        ui.horizontal(|ui| {
            ui.label(RichText::new("Sleep below").color(MUTED));
            ui.add(egui::DragValue::new(&mut self.s.threshold_ma).range(0.0001..=10_000.0).speed(0.01).max_decimals(4).suffix(" mA"))
                .on_hover_text("Current threshold: below = sleep, at or above = active");
            ui.checkbox(&mut self.s.show_threshold, "show").on_hover_text("Draw the threshold on the current plot");
        });
        let split = self.state_split();
        let dt = self.dt();
        if let Some(((nl, sl), (nh, sh))) = split {
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
                        let (rect, _) = ui.allocate_exact_size(vec2(10.0, 10.0), egui::Sense::hover());
                        ui.painter().circle_filled(rect.center(), 4.0, col);
                        ui.label(RichText::new(name).color(col));
                    });
                    ui.label(pct(n as f64, total_n)).on_hover_text(fmt::duration(n as f64 * dt));
                    ui.label(avg(sum, n));
                    ui.label(pct(sum, total_q)).on_hover_text(fmt::si(sum * 1e-6 * dt / 3600.0, "Ah"));
                    ui.end_row();
                }
            });
        } else {
            ui.label(RichText::new("No data").color(MUTED));
        }
        ui.add_space(6.0);
        ui.horizontal(|ui| {
            ui.label(RichText::new("Battery").color(MUTED));
            ui.add(egui::DragValue::new(&mut self.s.battery_mah).range(0.1..=1_000_000.0).speed(1.0).max_decimals(1).suffix(" mAh"))
                .on_hover_text("Battery capacity, e.g. CR2032 ≈ 220 mAh, AA ≈ 2500 mAh");
            let life = self.scope_agg().map(|g| g.sum / g.count as f64).filter(|avg| *avg > 0.0).map(|avg_ua| {
                let hours = self.s.battery_mah * 1000.0 / avg_ua;
                fmt_runtime(hours)
            });
            ui.label(RichText::new(format!("lasts ≈ {}", life.unwrap_or_else(|| "—".into()))).strong().color(GREEN));
        });
        ui.label(
            RichText::new(format!("Based on the average current of the {scope_name}; ignores self-discharge and temperature."))
                .size(11.0)
                .color(MUTED),
        );
    }

    fn markers_ui(&mut self, ui: &mut Ui) {
        if self.markers.is_empty() {
            ui.label(
                RichText::new("Press M while pointing at a plot, or right-click a plot and choose Add marker. While recording, M marks the current moment. Drag a marker line to move it.")
                    .size(12.0)
                    .color(MUTED),
            );
            return;
        }
        let scale_time = self.s.x_time;
        let dt = self.dt();
        let mut go = None;
        let mut select = None;
        let mut delete = None;
        let n = self.markers.len();
        egui::Grid::new("markers").num_columns(4).spacing([6.0, 4.0]).show(ui, |ui| {
            for k in 0..n {
                let r = ui.add_sized([130.0, 24.0], egui::TextEdit::singleline(&mut self.markers[k].label).text_color(MARKER_C))
                    .on_hover_text("Click to rename");
                // focus the name of a marker just added from the menu (the closing menu can
                // take focus away again in the first frames, so keep asking for a few frames)
                if self.focus_marker == Some(k) {
                    self.focus_tries += 1;
                    if r.has_focus() || self.focus_tries > 20 {
                        self.focus_marker = None;
                    } else {
                        r.request_focus();
                        // select the default name so typing replaces it
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
                let x = self.markers[k].x;
                let t = if scale_time { fmt::si(x * dt, "s") } else { format!("#{}", x as i64) };
                if ui.add(egui::Button::new(RichText::new(t).monospace().size(12.0)).frame(false)).on_hover_text("Show in plot").clicked() {
                    go = Some(k);
                }
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 3.0;
                    if k + 1 < n && ui.small_button("➡").on_hover_text("Select from this marker to the next one").clicked() {
                        select = Some(k);
                    }
                    if ui.small_button("🗙").on_hover_text("Delete marker").clicked() {
                        delete = Some(k);
                    }
                });
                ui.end_row();
            }
        });
        if let Some(k) = go {
            let x = self.markers[k].x;
            let half = (self.view.1 - self.view.0) / 2.0;
            self.view = (x - half, x + half);
            self.s.follow = false;
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

    fn stats_tiles(&self, ui: &mut Ui) {
        let g = self.scope_agg();
        let (dt, v) = (self.dt(), self.s.voltage);
        let dash = || "—".to_string();
        let vals: [(&str, String, Color32); 10] = match g {
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
            None => [
                ("DURATION", dash(), TEXT),
                ("SAMPLES", "0".into(), TEXT),
                ("AVG CURRENT", dash(), CURRENT_C),
                ("RMS CURRENT", dash(), CURRENT_C),
                ("PEAK CURRENT", dash(), CURRENT_C),
                ("MIN CURRENT", dash(), CURRENT_C),
                ("AVG POWER", dash(), POWER_C),
                ("PEAK POWER", dash(), POWER_C),
                ("CHARGE", dash(), GREEN),
                ("ENERGY", dash(), GREEN),
            ],
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
        let xs = self.store.x[k];
        let where_ = if self.s.x_time { fmt::si(xs * self.dt(), "s") } else { format!("#{}", xs as i64) };
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing.x = 10.0;
            ui.label(RichText::new(where_).monospace().color(MUTED));
            let i = self.store.raw.v[k];
            ui.label(RichText::new(format!("I = {}", fmt::si(i * 1e-6, "A"))).monospace().color(CURRENT_C));
            ui.label(RichText::new(format!("P = {}", fmt::si(i * self.s.voltage * 1e-6, "W"))).monospace().color(POWER_C));
        });
        if self.store.filter_active() {
            let f = self.store.filt.v[k];
            ui.horizontal_wrapped(|ui| {
                ui.spacing_mut().item_spacing.x = 10.0;
                ui.label(RichText::new("filtered").monospace().color(MUTED));
                ui.label(RichText::new(format!("I = {}", fmt::si(f * 1e-6, "A"))).monospace().color(CURRENT_C.gamma_multiply(0.8)));
                ui.label(RichText::new(format!("P = {}", fmt::si(f * self.s.voltage * 1e-6, "W"))).monospace().color(POWER_C.gamma_multiply(0.8)));
            });
        }
    }

    fn plots(&mut self, ui: &mut Ui) {
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
        let plots_h = ui.available_height() - ov_space - header_h * (1 + self.s.show_power as usize) as f32
            - if self.s.show_power { 8.0 } else { 0.0 };
        let h_i = if self.s.show_power { plots_h * 0.58 } else { plots_h };
        let desc = self.s.filter.describe(self.dt());
        let raw = with_raw.then_some(decim_raw.as_slice());
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
            eprintln!("plots: {:.2} ms, {} points, view {:.0} samples", t0.elapsed().as_secs_f64() * 1e3, decim.len(), self.view.1 - self.view.0);
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
        let factor = if id == 0 { 1e-6 } else { self.s.voltage.max(1e-12) * 1e-6 };
        let (unit, color) = if id == 0 { ("A", CURRENT_C) } else { ("W", POWER_C) };

        // ----- selection dragging (decided before the plot so it can disable panning)
        let (pointer, pressed, down, shift) =
            ui.input(|i| (i.pointer.interact_pos(), i.pointer.primary_pressed(), i.pointer.primary_down(), i.modifiers.shift));
        // where the button went down (press and moves can arrive in the same frame)
        let press_pos = if pressed { ui.input(|i| i.pointer.press_origin()).or(pointer) } else { None };
        if std::env::var_os("ANALIZATOR_DEBUG").is_some() && pressed {
            eprintln!("press id={id} at={press_pos:?} shift={shift}");
        }
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
            let marker_at = |px: f32| self.markers.iter().position(|m| (tf.position_from_point_x(m.x * scale) - px).abs() < 6.0);
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
        let bounds = PlotBounds::from_min_max([self.view.0 * scale, yr.0 * factor], [self.view.1 * scale, yr.1 * factor]);
        let sel = (self.scope == Scope::Selection).then(|| (self.sel.0 * scale, self.sel.1 * scale));
        let hover = self.hover_x.and_then(|x| self.store.nearest(x)).map(|k| self.store.x[k] * scale);
        let x_time = self.s.x_time;
        let dragging_sel = self.grab != Grab::None;
        let marks: Vec<(f64, String)> = self.markers.iter().map(|m| (m.x * scale, m.label.clone())).collect();
        let label_y = yr.1 * factor;
        let threshold = (id == 0 && self.s.show_threshold && !self.store.is_empty()).then(|| self.s.threshold_ma * 1e-3);

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
            .x_axis_formatter(move |m, r| x_label_edge(m.value, r, x_time))
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
                    pu.hline(HLine::new("", t).color(AMBER.gamma_multiply(0.7)).width(1.0).style(egui_plot::LineStyle::dashed_dense()));
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
        if r.secondary_clicked() {
            if let Some(p) = r.interact_pointer_pos() {
                let near = self.markers.iter().position(|m| (tf.position_from_point_x(m.x * scale) - p.x).abs() < 6.0);
                self.ctx_menu = Some((tf.value_from_position(p).x / scale, near));
            }
        }
        if let Some((cx, near)) = self.ctx_menu {
            let mut action = 0;
            let has_markers = !self.markers.is_empty();
            let near_label = near.and_then(|k| self.markers.get(k)).map(|m| m.label.clone());
            r.context_menu(|ui| {
                if ui.button("Add marker here").clicked() {
                    action = 1;
                }
                if let Some(l) = &near_label {
                    if ui.button(format!("Delete marker “{l}”")).clicked() {
                        action = 2;
                    }
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
                    self.add_marker(cx.round());
                    // added on purpose from the menu: let the user type a name right away
                    self.focus_marker = self.markers.iter().position(|m| m.x == self.clamp_x(cx.round()));
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
        let hover = self.hover_x.and_then(|x| self.store.nearest(x)).map(|k| self.store.x[k] * scale);
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
            .x_axis_formatter(move |m, r| x_label_edge(m.value, r, x_time))
            .y_axis_formatter(|_, _| String::new())
            .show(ui, move |pu| {
                pu.set_plot_bounds(bounds);
                pu.line(Line::new("", PlotPoints::new(pts)).color(CURRENT_C.gamma_multiply(0.75)).width(1.0));
                pu.span(Span::new("", view.0..=view.1).fill(ACCENT.gamma_multiply(0.18)).border(Stroke::new(1.0, ACCENT)));
                for mx in marks {
                    pu.vline(VLine::new("", mx).color(MARKER_C.gamma_multiply(0.8)).width(1.0));
                }
                if let Some(hx) = hover {
                    pu.vline(VLine::new("", hx).color(MUTED).width(1.0));
                }
            });
        let r = &resp.response;
        if (r.dragged() || r.clicked()) && !self.store.is_empty() {
            if let Some(p) = r.interact_pointer_pos() {
                let xv = resp.transform.value_from_position(p).x / scale;
                let w = self.view.1 - self.view.0;
                self.view = (xv - w / 2.0, xv + w / 2.0);
                self.s.follow = false;
            }
        }
        if r.hovered() {
            ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
        }
        r.hovered()
    }

    fn dialogs(&mut self, ctx: &egui::Context) {
        // file picker
        self.file_dialog.update(ctx);
        if let Some(p) = self.file_dialog.take_picked() {
            self.file_picked(p);
        } else if self.file_action.is_some() && *self.file_dialog.state() != egui_file_dialog::DialogState::Open {
            self.file_action = None;
        }

        // confirmations
        if let Some(which) = self.confirm {
            let (title, text, ok) = match which {
                Confirm::Erase => ("Erase flash", "Erase all samples stored in the board's flash memory?\nThis cannot be undone.", "Erase"),
                Confirm::DumpReplace => ("Dump flash", "Dumping replaces the data currently shown.\nContinue?", "Dump"),
            };
            let mut answer = None;
            let m = egui::Modal::new(Id::new("confirm")).show(ctx, |ui| {
                ui.set_width(360.0);
                ui.heading(title);
                ui.add_space(6.0);
                ui.label(text);
                ui.add_space(12.0);
                ui.horizontal(|ui| {
                    if button(ui, true, ok, Some(if which == Confirm::Erase { BTN_RED } else { BTN_AMBER }), "", 80.0) {
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
                        Confirm::Erase => {
                            if self.send("ERASE") {
                                self.status = "Erase command sent.".into();
                            }
                        }
                        Confirm::DumpReplace => self.start_dump(),
                    }
                }
            }
        }

        // errors
        if let Some((title, text)) = self.error_popup.clone() {
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

        if self.recording() || self.busy.is_some() {
            ctx.request_repaint_after(Duration::from_millis(33));
        } else {
            ctx.request_repaint_after(Duration::from_millis(1000));
        }
    }

    fn save(&mut self, storage: &mut dyn eframe::Storage) {
        eframe::set_value(storage, eframe::APP_KEY, &self.s);
    }
}

// ------------------------------------------------------------- widgets ---

fn button(ui: &mut Ui, enabled: bool, text: &str, fill: Option<Color32>, tip: &str, min_w: f32) -> bool {
    let mut rt = RichText::new(text).strong();
    if fill.is_some() {
        rt = rt.color(Color32::WHITE);
    }
    let mut b = egui::Button::new(rt).min_size(vec2(min_w, 30.0)).corner_radius(CornerRadius::same(6));
    if let Some(f) = fill {
        b = b.fill(f).stroke(Stroke::new(1.0, f.lerp_to_gamma(Color32::WHITE, 0.15)));
    }
    let r = ui.add_enabled(enabled, b);
    let r = if tip.is_empty() { r } else { r.on_hover_text(tip) };
    r.clicked()
}

fn vsep(ui: &mut Ui) {
    ui.add_space(4.0);
    let (rect, _) = ui.allocate_exact_size(vec2(1.0, 24.0), egui::Sense::hover());
    ui.painter().rect_filled(rect, 0.0, BORDER);
    ui.add_space(4.0);
}

/// Collapsible sidebar section (open/closed state is remembered).
fn section(ui: &mut Ui, title: &str, add: impl FnOnce(&mut Ui)) {
    egui::CollapsingHeader::new(RichText::new(title).size(11.0).strong().color(MUTED))
        .id_salt(title)
        .default_open(true)
        .show_unindented(ui, |ui| group(ui, "", add));
}

fn group(ui: &mut Ui, title: &str, add: impl FnOnce(&mut Ui)) {
    if !title.is_empty() {
        ui.label(RichText::new(title).size(11.0).strong().color(MUTED));
    }
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

fn tile(ui: &mut Ui, title: &str, value: &str, color: Color32, width: f32) {
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

fn plot_header(ui: &mut Ui, title: &str, color: Color32, filter: &str) {
    ui.horizontal(|ui| {
        ui.add_space(74.0);
        let (rect, _) = ui.allocate_exact_size(vec2(10.0, 10.0), egui::Sense::hover());
        ui.painter().circle_filled(rect.center(), 4.0, color);
        ui.label(RichText::new(title).strong().size(11.5).color(MUTED));
        if !filter.is_empty() {
            ui.add_space(6.0);
            ui.label(RichText::new(format!("filtered: {filter}")).size(11.5).color(ACCENT));
        }
    });
}

/// Like `x_label`, but leaves out the label at the very right edge (it would be cut off).
fn x_label_edge(v: f64, range: &std::ops::RangeInclusive<f64>, time: bool) -> String {
    let span = range.end() - range.start();
    if v > range.end() - span * 0.025 {
        return String::new();
    }
    x_label(v, time)
}

/// Battery runtime: "45 min", "17.5 h", "41 days", "2.3 years".
fn fmt_runtime(hours: f64) -> String {
    if !hours.is_finite() {
        return "—".into();
    }
    if hours < 1.0 {
        format!("{:.0} min", hours * 60.0)
    } else if hours < 48.0 {
        format!("{} h", fmt::sig(hours, 3))
    } else if hours < 24.0 * 730.0 {
        format!("{} days", fmt::sig(hours / 24.0, 3))
    } else {
        format!("{} years", fmt::sig(hours / 24.0 / 365.25, 3))
    }
}

fn x_label(v: f64, time: bool) -> String {
    if !time {
        return fmt::sig(v, 9);
    }
    if v.abs() < 1e-12 {
        return "0".into();
    }
    if v.abs() < 60.0 {
        return fmt::si_digits(v, "s", 4);
    }
    // minutes:seconds or hours:minutes:seconds
    let sign = if v < 0.0 { "-" } else { "" };
    let t = v.abs();
    let whole = t.round();
    let (h, m, s) = ((whole / 3600.0).floor(), ((whole % 3600.0) / 60.0).floor(), whole % 60.0);
    let frac = if (t - whole).abs() > 1e-6 { fmt::sig(t - t.floor(), 3).trim_start_matches('0').to_string() } else { String::new() };
    if h > 0.0 { format!("{sign}{h}:{m:02}:{s:02}{frac}") } else { format!("{sign}{m}:{s:02}{frac}") }
}

fn file_name(p: &Path) -> String {
    p.file_name().map(|s| s.to_string_lossy().to_string()).unwrap_or_else(|| p.display().to_string())
}

/// `logs` next to the program (e.g. on a USB stick) if that folder is visible and writable,
/// otherwise ~/Analizator/logs (e.g. when installed into ~/.local).
fn pick_logs_dir() -> PathBuf {
    let exe_dir = std::env::current_exe().ok().and_then(|p| p.parent().map(|d| d.to_path_buf()));
    let usable = exe_dir.as_ref().is_some_and(|d| {
        let s = d.to_string_lossy();
        let lower = s.to_lowercase();
        !s.contains("/.") && !s.starts_with("/usr") && !s.starts_with("/opt") && !lower.contains("\\program files")
    });
    if let Some(dir) = exe_dir.filter(|_| usable).map(|d| d.join("logs")) {
        if std::fs::create_dir_all(&dir).is_ok() {
            let probe = dir.join(".write_test");
            if std::fs::write(&probe, b"").is_ok() {
                let _ = std::fs::remove_file(probe);
                return dir;
            }
        }
    }
    let home = std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."));
    home.join("Analizator").join("logs")
}

/// Show a folder in the system file manager.
fn open_folder(dir: &Path) {
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

fn apply_theme(ctx: &egui::Context) {
    // always use our dark theme, even when the system (e.g. Windows) is set to light mode
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
    let r = CornerRadius::same(6);
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
        s.corner_radius = r;
        s.expansion = 0.0;
    }
    ctx.set_visuals(v);
    ctx.all_styles_mut(|s| {
        s.spacing.item_spacing = vec2(8.0, 6.0);
        s.spacing.button_padding = vec2(10.0, 5.0);
        s.spacing.interact_size = vec2(40.0, 26.0);
        s.spacing.combo_height = 300.0;
        use egui::{FontId, TextStyle};
        s.text_styles.insert(TextStyle::Body, FontId::proportional(13.5));
        s.text_styles.insert(TextStyle::Button, FontId::proportional(13.5));
        s.text_styles.insert(TextStyle::Monospace, FontId::monospace(12.5));
        s.text_styles.insert(TextStyle::Heading, FontId::proportional(18.0));
        s.text_styles.insert(TextStyle::Small, FontId::proportional(11.0));
    });
}
