//! Device list, connecting (on a background thread), reconnecting and recording.
//! Only talks to devices through the `Device` trait.

use std::sync::mpsc::channel;
use std::time::{Duration, Instant};

use super::{AnalyzerApp, Confirm, Lost, Mode, Opening, files};
use crate::data::MAX_SAMPLES;
use crate::device::{self, Device, DeviceEntry, DeviceEvent, Msg, OpenOptions, PanelCtx};
use crate::fmt;
use crate::ui::theme::{ACCENT, AMBER, GREEN, MUTED, TEXT};

const DUMP_IDLE_TIMEOUT: Duration = Duration::from_millis(1500);
const DUMP_START_TIMEOUT: Duration = Duration::from_secs(15);
const RECONNECT_INTERVAL: Duration = Duration::from_secs(1);
const SCAN_INTERVAL: Duration = Duration::from_millis(1500);

impl AnalyzerApp {
    pub(super) fn selected_entry(&self) -> Option<&DeviceEntry> {
        self.devices.iter().find(|e| e.id == self.device_sel)
    }

    pub(super) fn rescan(&mut self) {
        let list = device::discover();
        if list != self.devices {
            // keep the selection; if that device vanished, prefer one that just appeared
            if !list.iter().any(|e| e.id == self.device_sel)
                && let Some(e) = list.iter().find(|e| !self.devices.contains(e)).or(list.first())
            {
                self.device_sel = e.id.clone();
            }
            self.devices = list;
        }
    }

    fn open_options(&self, restore: Option<String>) -> OpenOptions {
        OpenOptions {
            baud: self.s.baud,
            sample_period_s: self.s.sample_period_ms / 1000.0,
            prefs: self.s.device_prefs.clone(),
            restore,
        }
    }

    /// Connect button: connect, disconnect, or cancel connecting/reconnecting.
    pub(super) fn toggle_connection(&mut self, ctx: &egui::Context) {
        match self.mode {
            Mode::Connecting | Mode::Reconnecting => {
                self.opening = None; // a device still being opened is closed by its thread
                self.lost = None;
                self.mode = Mode::Offline;
                self.log("Cancelled.", MUTED);
                self.status = "Disconnected".into();
            }
            _ if self.dev.is_some() => self.disconnect(),
            _ => self.connect(ctx),
        }
    }

    fn connect(&mut self, ctx: &egui::Context) {
        let Some(entry) = self.selected_entry().cloned() else {
            self.show_error("No device", "Choose a device first.");
            return;
        };
        let uses_baud = device::driver(entry.driver).is_some_and(|d| d.uses_baud_rate());
        if uses_baud {
            match self.baud_text.trim().parse::<u32>() {
                Ok(b) if b >= 300 => self.s.baud = b,
                _ => {
                    self.show_error("Invalid baud rate", format!("'{}' is not a valid baud rate.", self.baud_text));
                    return;
                }
            }
        }
        self.status = format!("Connecting to {}…", entry.label);
        self.mode = Mode::Connecting;
        self.start_open(entry, None, false, ctx);
    }

    /// Open a device without freezing the window (a PPK2 handshake can take seconds).
    fn start_open(&mut self, entry: DeviceEntry, restore: Option<String>, reconnect: bool, ctx: &egui::Context) {
        let (tx, rx) = channel();
        let opts = self.open_options(restore);
        let ctx2 = ctx.clone();
        std::thread::spawn(move || {
            let r = device::open(&entry, &opts, &ctx2);
            if let Err(std::sync::mpsc::SendError((_, Ok(dev)))) = tx.send((entry, r)) {
                dev.close(); // nobody is waiting any more (cancelled)
            }
            ctx2.request_repaint();
        });
        self.opening = Some(Opening { rx, reconnect });
    }

    fn on_opened(&mut self, entry: DeviceEntry, result: Result<Box<dyn Device>, String>, op: Opening) {
        match result {
            Ok(dev) => {
                let name = dev.name();
                self.dev = Some(dev);
                self.mode = Mode::Connected;
                self.device_sel = entry.id.clone();
                self.s.last_device = entry.id;
                self.lost = None;
                if op.reconnect {
                    self.log(format!("Reconnected to {name}. Press Record to continue."), GREEN);
                    self.status = format!("Reconnected to {name}");
                } else {
                    self.log(format!("Connected to {name}"), GREEN);
                    self.status = format!("Connected to {name}");
                }
                self.sync_caps();
            }
            Err(e) if op.reconnect => {
                // not ready yet (e.g. still enumerating); try again in a second
                if let Some(l) = &mut self.lost {
                    l.last_try = Instant::now();
                }
                let _ = e;
            }
            Err(e) => {
                self.mode = Mode::Offline;
                self.status = "Connection failed".into();
                self.show_error("Connection error", e);
            }
        }
    }

    fn disconnect(&mut self) {
        match self.mode {
            Mode::Capture => self.finish_capture(),
            Mode::Dump => self.finish_dump(),
            _ => {}
        }
        if let Some(d) = self.dev.take() {
            d.close();
        }
        if self.store.is_empty() {
            (self.period_ms, self.voltage) = (self.s.sample_period_ms, self.s.dut_voltage);
        }
        self.mode = Mode::Offline;
        self.log("Disconnected", MUTED);
        self.status = "Disconnected".into();
    }

    /// The link failed (usually: USB unplugged). Save what we have and wait for the device to return.
    fn connection_lost(&mut self, err: String) {
        let was_recording = self.recording();
        match self.mode {
            Mode::Capture => self.finish_capture(),
            Mode::Dump => self.finish_dump(),
            _ => {}
        }
        let Some(dev) = self.dev.take() else { return };
        let (name, entry, state) = (dev.name(), dev.entry().clone(), dev.save_state());
        dev.close();
        self.log(format!("Connection to {name} lost ({err}). Waiting for the device to come back…"), AMBER);
        if was_recording {
            self.log("The recording up to this point was saved.", AMBER);
        }
        self.status = format!("Connection lost — waiting for {name} to come back (press Cancel to stop)");
        self.lost = Some(Lost { entry, state, last_try: Instant::now() });
        self.mode = Mode::Reconnecting;
    }

    /// Sample period (ms) and supply voltage (V) a new recording on the current device uses.
    fn device_period_and_voltage(&self) -> (f64, f64) {
        let caps = self.dev.as_ref().map(|d| d.caps()).unwrap_or_default();
        (
            caps.fixed_sample_period.map(|p| p * 1000.0).unwrap_or(self.s.sample_period_ms),
            caps.supply_voltage.unwrap_or(self.s.dut_voltage),
        )
    }

    /// While nothing is shown (or while recording), the data takes the device's
    /// period and voltage. Data loaded from a file keeps its own.
    fn sync_caps(&mut self) {
        if self.dev.is_none() || !(self.store.is_empty() || self.recording()) {
            return;
        }
        let (p, v) = self.device_period_and_voltage();
        if self.store.is_empty() {
            self.period_ms = p;
        }
        self.voltage = v;
    }

    pub(super) fn panel_ctx(&self) -> PanelCtx {
        PanelCtx { recording: self.recording(), has_data: !self.store.is_empty() }
    }

    /// Messages from the device, device events, reconnecting, port watching, dump timeout.
    pub(super) fn poll_device(&mut self, ctx: &egui::Context) {
        // finished opening?
        if let Some(op) = self.opening.take() {
            match op.rx.try_recv() {
                Ok((entry, r)) => self.on_opened(entry, r, op),
                Err(std::sync::mpsc::TryRecvError::Empty) => self.opening = Some(op),
                Err(_) => {
                    if !op.reconnect {
                        self.mode = Mode::Offline;
                    }
                }
            }
        }
        // samples, lines, errors
        let mut msgs = Vec::new();
        if let Some(d) = &self.dev {
            while let Some(m) = d.link().try_recv() {
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
        // events from the device (its settings panel, …)
        let events = self.dev.as_mut().map(|d| d.take_events()).unwrap_or_default();
        for ev in events {
            match ev {
                DeviceEvent::Log(level, text) => self.log_level(level, text),
                DeviceEvent::ClearData => self.clear_data(),
                DeviceEvent::SetPref(k, v) => {
                    self.s.device_prefs.insert(k, v);
                }
            }
        }
        self.sync_caps();
        // waiting for a lost device
        if self.mode == Mode::Reconnecting && self.opening.is_none() {
            let due = self.lost.as_ref().is_some_and(|l| l.last_try.elapsed() >= RECONNECT_INTERVAL);
            if due {
                self.devices = device::discover();
                let lost = self.lost.as_mut().expect("checked above");
                lost.last_try = Instant::now();
                if let Some(e) = device::find_again(&lost.entry, &self.devices) {
                    let state = lost.state.clone();
                    self.start_open(e, state, true, ctx);
                }
            }
        }
        // watch for plugged/unplugged adapters
        if self.dev.is_none() && self.mode == Mode::Offline && self.last_scan.elapsed() > SCAN_INTERVAL {
            self.last_scan = Instant::now();
            self.rescan();
        }
        // a flash dump has no end marker: finish after a quiet period
        if self.mode == Mode::Dump {
            let done = match self.last_data {
                Some(t) => t.elapsed() > DUMP_IDLE_TIMEOUT,
                None => self.dump_t0.elapsed() > DUMP_START_TIMEOUT,
            };
            if done {
                self.finish_dump();
            }
        }
    }

    /// Run a device command and log it ("> START" or the error).
    fn device_cmd(&mut self, what: &str, f: impl FnOnce(&mut dyn Device) -> Result<(), String>) -> bool {
        let Some(d) = self.dev.as_mut() else { return false };
        match f(d.as_mut()) {
            Ok(()) => {
                self.log(format!("> {what}"), ACCENT);
                true
            }
            Err(e) => {
                self.show_error("Device", e);
                false
            }
        }
    }

    // ---------------------------------------------------------- capture ---
    pub(super) fn toggle_record(&mut self) {
        if self.recording() {
            self.stop();
        } else if self.connected() && self.busy.is_none() && matches!(self.mode, Mode::Connected) {
            self.start_capture();
        }
    }

    pub(super) fn start_capture(&mut self) {
        // a recording continues the data shown only if it has the same sample rate
        let (p, v) = self.device_period_and_voltage();
        if !self.store.is_empty() && (p - self.period_ms).abs() > 1e-9 * p {
            self.clear_data();
            self.log("Cleared the data shown: it has a different sample rate than this device.", MUTED);
        }
        self.period_ms = p;
        self.voltage = v;
        if !self.device_cmd("START", |d| d.start()) {
            return;
        }
        if let Some(d) = &self.dev {
            d.link().reset_counts();
        }
        self.mode = Mode::Capture;
        self.record_start = self.store.next_index;
        self.capture_t0 = Instant::now();
        self.full_warned = false;
        self.s.follow = true;
        self.status = "Recording…".into();
    }

    pub(super) fn stop(&mut self) {
        self.device_cmd("STOP", |d| d.stop());
        match self.mode {
            Mode::Capture => self.finish_capture(),
            Mode::Dump => self.finish_dump(),
            _ => {}
        }
    }

    fn after_recording_mode(&self) -> Mode {
        if self.dev.is_some() { Mode::Connected } else { Mode::Offline }
    }

    fn log_counts(&mut self) {
        let Some(d) = &self.dev else { return };
        let (rx, bad) = d.link().counts();
        let (n_rx, n_bad) = d.counter_names();
        let col = if bad > 0 { AMBER } else { MUTED };
        self.log(format!("{} {n_rx} received, {} {n_bad}", fmt::count(rx as usize), fmt::count(bad as usize)), col);
    }

    fn finish_capture(&mut self) {
        self.log_counts();
        self.mode = self.after_recording_mode();
        self.fit();
        let (a, b) = self.store.index_range(self.record_start, f64::INFINITY);
        if b > a {
            let path = self.logs_dir.join(files::capture_name("power_capture_log"));
            self.save_async(path, a, b, false);
        } else {
            self.status = "Capture stopped (no samples received).".into();
        }
    }

    pub(super) fn request_dump(&mut self) {
        if self.store.is_empty() {
            self.start_dump();
        } else {
            self.confirm = Some(Confirm::DumpReplace);
        }
    }

    pub(super) fn start_dump(&mut self) {
        self.clear_data();
        if !self.device_cmd("DUMP", |d| d.dump_flash()) {
            return;
        }
        if let Some(d) = &self.dev {
            d.link().reset_counts();
        }
        self.mode = Mode::Dump;
        self.record_start = 0.0;
        self.dump_t0 = Instant::now();
        self.last_data = None;
        self.s.follow = true;
        self.status = "Dumping flash…".into();
    }

    fn finish_dump(&mut self) {
        self.log_counts();
        self.mode = self.after_recording_mode();
        self.fit();
        let n = self.store.len();
        if n > 0 {
            self.log(format!("Flash dump finished: {} samples", fmt::count(n)), GREEN);
            let path = self.logs_dir.join(files::capture_name("flash_dump"));
            self.save_async(path, 0, n, false);
        } else {
            self.log("Flash dump: no data received", AMBER);
            self.status = "Flash dump: no data received.".into();
        }
    }

    pub(super) fn erase_flash(&mut self) {
        if self.device_cmd("ERASE", |d| d.erase_flash()) {
            self.status = "Erase command sent.".into();
        }
    }

    pub(super) fn set_sample_period_on_device(&mut self) {
        let dt = self.dt();
        if let Some(d) = self.dev.as_mut() {
            d.set_sample_period(dt);
        }
    }

    fn on_data(&mut self, chunk: Vec<f64>) {
        if !self.recording() {
            return; // devices may be chatty outside a capture; ignore
        }
        let stored = self.store.append(&chunk);
        if stored < chunk.len() && !self.full_warned {
            self.full_warned = true;
            self.log(
                format!("Memory limit reached ({} samples) — stopping.", fmt::count(MAX_SAMPLES)),
                crate::ui::theme::RED,
            );
            self.stop();
        }
        self.last_data = Some(Instant::now());
        self.rate_count += stored;
    }

    pub(super) fn clear_data(&mut self) {
        if self.mode == Mode::Capture {
            self.record_start = 0.0;
        }
        self.store.clear();
        self.markers.clear();
        self.marker_seq = 0;
        self.sel_init = false;
        self.hover_x = None;
        self.title.clear();
        let (p, v) = self.device_period_and_voltage();
        (self.period_ms, self.voltage) = (p, v);
        self.view = (0.0, self.s.live_window_s / self.dt());
        self.status = "Cleared.".into();
    }

    pub(super) fn fit(&mut self) {
        if self.store.is_empty() {
            return;
        }
        self.s.follow = false;
        let (x0, x1) = (self.store.first_x(), self.store.last_x());
        let pad = ((x1 - x0) * 0.01).max(1.0);
        self.view = (x0 - pad, x1 + pad);
    }
}
