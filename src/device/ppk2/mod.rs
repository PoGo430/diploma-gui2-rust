//! Nordic Power Profiler Kit II: driver, device and settings panel.

pub mod protocol;

use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

use egui::{RichText, Ui};

use self::protocol::{Live, Meta, Ppk2Decoder, SAMPLE_RATE, VDD_MAX_MV, VDD_MIN_MV};
use super::link::{ByteWriter, Link};
use super::ports::{self, SystemPorts};
use super::{Capabilities, Device, DeviceEntry, DeviceEvent, Driver, Level, OpenOptions, PanelCtx};
use crate::data::MAX_SAMPLES;
use crate::fmt;
use crate::ui::theme::{BTN_GREEN, BTN_RED, MUTED};
use crate::ui::widgets::{button, caption, hint};

const PREF_DECIMATION: &str = "ppk2.decimation";
/// (raw samples averaged into one, label)
const RATES: [(u32, &str); 3] = [(1, "100 kHz"), (10, "10 kHz"), (100, "1 kHz")];

/// Mode, voltage and power as last sent to the PPK2.
#[derive(Clone, Copy, Debug, PartialEq)]
struct State {
    source_meter: bool,
    dut_power: bool,
    vdd_mv: u32,
}

impl State {
    fn encode(&self) -> String {
        format!("source={};power={};vdd={}", self.source_meter as u8, self.dut_power as u8, self.vdd_mv)
    }

    fn decode(s: &str) -> Option<State> {
        let mut st = State { source_meter: false, dut_power: false, vdd_mv: 3300 };
        for part in s.split(';') {
            let (k, v) = part.split_once('=')?;
            match k {
                "source" => st.source_meter = v == "1",
                "power" => st.dut_power = v == "1",
                "vdd" => st.vdd_mv = v.parse().ok()?,
                _ => {}
            }
        }
        Some(st)
    }
}

pub struct Ppk2Device {
    entry: DeviceEntry,
    port: String,
    link: Link,
    writer: Box<dyn ByteWriter>,
    live: Arc<Live>,
    meta: Meta,
    state: State,
    decimation: u32,
    events: Vec<DeviceEvent>,
    /// Supply voltage being edited (V); sent when editing ends.
    vdd_edit: f64,
    /// Sample rate waiting for "clear the data?" confirmation.
    pending_rate: Option<u32>,
}

impl Ppk2Device {
    fn send(&mut self, bytes: &[u8]) -> bool {
        match self.writer.write(bytes) {
            Ok(()) => true,
            Err(e) => {
                self.log(Level::Error, format!("PPK2 command failed: {e}"));
                false
            }
        }
    }

    fn log(&mut self, level: Level, text: impl Into<String>) {
        self.events.push(DeviceEvent::Log(level, text.into()));
    }

    /// Send mode, voltage and power.
    fn apply(&mut self, st: State) {
        let vdd = st.vdd_mv.clamp(VDD_MIN_MV, VDD_MAX_MV);
        self.live.vdd_mv.store(vdd, Ordering::Relaxed);
        let ok = self.send(&protocol::power_mode_cmd(st.source_meter))
            && self.send(&protocol::regulator_cmd(vdd))
            && self.send(&protocol::dut_power_cmd(st.dut_power));
        if ok {
            self.state = State { vdd_mv: vdd, ..st };
            self.vdd_edit = vdd as f64 / 1000.0;
        }
    }

    fn set_power(&mut self, on: bool) {
        if self.send(&protocol::dut_power_cmd(on)) {
            self.state.dut_power = on;
            self.log(
                if on { Level::Good } else { Level::Info },
                format!("Device power {}", if on { "ON" } else { "OFF" }),
            );
        }
    }

    fn set_rate(&mut self, decimation: u32) {
        self.decimation = decimation;
        self.live.decimation.store(decimation, Ordering::Relaxed);
        self.events.push(DeviceEvent::ClearData);
        self.events.push(DeviceEvent::SetPref(PREF_DECIMATION.into(), decimation.to_string()));
        self.log(Level::Info, format!("PPK2 sample rate: {}", fmt::si(SAMPLE_RATE / decimation as f64, "Hz")));
    }
}

impl Device for Ppk2Device {
    fn name(&self) -> String {
        format!("PPK2 on {}", self.port)
    }
    fn badge(&self) -> String {
        "PPK2".into()
    }
    fn entry(&self) -> &DeviceEntry {
        &self.entry
    }
    fn caps(&self) -> Capabilities {
        Capabilities {
            flash: false,
            baud_rate: false,
            fixed_sample_period: Some(self.decimation as f64 / SAMPLE_RATE),
            supply_voltage: self.state.source_meter.then_some(self.state.vdd_mv as f64 / 1000.0),
        }
    }
    fn link(&self) -> &Link {
        &self.link
    }

    fn start(&mut self) -> Result<(), String> {
        if !self.state.dut_power {
            self.set_power(true);
        }
        self.link.restart_decoder();
        self.writer.write(&[protocol::cmd::AVERAGE_START]).map_err(|e| e.to_string())
    }

    fn stop(&mut self) -> Result<(), String> {
        self.writer.write(&[protocol::cmd::AVERAGE_STOP]).map_err(|e| e.to_string())
    }

    fn counter_names(&self) -> (&'static str, &'static str) {
        ("PPK2 samples", "lost")
    }
    fn counter_help(&self) -> &'static str {
        "PPK2 samples lost on the way (detected from the sample counter) since the recording started. Gaps are filled with the previous value."
    }

    fn panel_title(&self) -> Option<&'static str> {
        Some("PPK2")
    }

    fn panel_ui(&mut self, ui: &mut Ui, ctx: &PanelCtx) {
        let st = self.state;
        ui.horizontal(|ui| {
            caption(ui, "Mode");
            ui.add_enabled_ui(!ctx.recording, |ui| {
                let mut source = st.source_meter;
                ui.selectable_value(&mut source, false, "Ampere meter")
                    .on_hover_text("The device runs from its own supply; the PPK2 measures the current in series.");
                ui.selectable_value(&mut source, true, "Source meter")
                    .on_hover_text("The PPK2 powers the device (0.8–5 V) and measures its current.");
                if source != st.source_meter {
                    // ampere meter needs the pass-through switched on; the source output starts off
                    self.apply(State { source_meter: source, dut_power: !source, ..st });
                    let m = if source { "source meter (output off)" } else { "ampere meter" };
                    self.log(Level::Info, format!("PPK2 mode: {m}"));
                }
            });
        });
        let st = self.state;
        if st.source_meter {
            ui.horizontal(|ui| {
                caption(ui, "Supply");
                let r = ui.add(
                    egui::DragValue::new(&mut self.vdd_edit)
                        .range(0.8..=5.0)
                        .speed(0.01)
                        .fixed_decimals(3)
                        .suffix(" V")
                        // never send half-typed values ("1" while typing "1.8") to the device
                        .update_while_editing(false),
                );
                let mv = (self.vdd_edit * 1000.0).round() as u32;
                if (r.drag_stopped() || (r.changed() && !r.dragged()))
                    && mv != st.vdd_mv
                    && self.send(&protocol::regulator_cmd(mv))
                {
                    self.live.vdd_mv.store(mv, Ordering::Relaxed);
                    self.state.vdd_mv = mv;
                    self.log(Level::Info, format!("PPK2 supply set to {}", fmt::si(mv as f64 / 1000.0, "V")));
                }
                r.on_hover_text("Voltage the PPK2 supplies to the device (0.8–5.0 V). Also used for power and energy.");
            });
        } else {
            hint(
                ui,
                "Set DUT voltage (Measurement section) to your device's supply voltage for correct power and energy.",
            );
        }
        ui.horizontal(|ui| {
            caption(ui, "Device power");
            let (txt, fill) = if st.dut_power { ("ON", BTN_GREEN) } else { ("OFF", BTN_RED) };
            if button(ui, true, txt, Some(fill), "Switch the power to the device under test on or off", 56.0) {
                self.set_power(!st.dut_power);
            }
        });
        ui.horizontal(|ui| {
            caption(ui, "Sample rate");
            ui.add_enabled_ui(!ctx.recording, |ui| {
                for (dec, name) in RATES {
                    if ui.selectable_label(self.decimation == dec, name).clicked() && self.decimation != dec {
                        if ctx.has_data {
                            self.pending_rate = Some(dec);
                        } else {
                            self.set_rate(dec);
                        }
                    }
                }
            });
        });
        if let Some(dec) = self.pending_rate {
            ui.horizontal(|ui| {
                ui.label(RichText::new("Changing the rate clears the data shown.").size(11.5).color(MUTED));
                if ui.small_button("Change").clicked() {
                    self.pending_rate = None;
                    self.set_rate(dec);
                }
                if ui.small_button("Cancel").clicked() {
                    self.pending_rate = None;
                }
            });
        }
        let rate = SAMPLE_RATE / self.decimation as f64;
        let mut text = format!("Memory is full after {} at this rate.", fmt::duration(MAX_SAMPLES as f64 / rate));
        if self.decimation > 1 {
            text.push_str(" Lower rates average the PPK2's 100 kHz samples.");
        }
        hint(ui, text);
    }

    fn take_events(&mut self) -> Vec<DeviceEvent> {
        std::mem::take(&mut self.events)
    }

    fn save_state(&self) -> Option<String> {
        Some(self.state.encode())
    }

    fn close(mut self: Box<Self>) {
        let _ = self.writer.write(&[protocol::cmd::AVERAGE_STOP]);
        self.link.close();
    }
}

/// Finds PPK2s by their USB id and shows each as one entry.
pub struct Ppk2Driver;

impl Driver for Ppk2Driver {
    fn id(&self) -> &'static str {
        "ppk2"
    }

    fn discover(&self, sys: &mut SystemPorts, out: &mut Vec<DeviceEntry>) {
        // the PPK2 has two serial ports: group them by serial number, first
        // interface first (like Nordic's app), and keep the other as a fallback
        let mut ports: Vec<_> = sys.ports.iter().filter(|p| p.is_usb(protocol::VID, protocol::PID)).cloned().collect();
        ports.sort_by(|a, b| (&a.serial, a.interface, &a.path).cmp(&(&b.serial, b.interface, &b.path)));
        for p in &ports {
            sys.claim(&p.path);
        }
        for group in ports.chunk_by(|a, b| a.serial == b.serial) {
            let first = &group[0];
            let serial = &first.serial;
            let short =
                if serial.is_empty() { String::new() } else { format!(" ({})", &serial[..serial.len().min(8)]) };
            out.push(DeviceEntry {
                driver: self.id(),
                id: first.path.clone(),
                label: format!("PPK2{short}  —  {}", first.path),
                detail: format!("Nordic Power Profiler Kit II, serial {serial}"),
                paths: group.iter().map(|p| p.path.clone()).collect(),
                match_key: format!("ppk2:{serial}"),
            });
        }
    }

    fn open(&self, entry: &DeviceEntry, opts: &OpenOptions, ctx: &egui::Context) -> Result<Box<dyn Device>, String> {
        let decimation = opts
            .prefs
            .get(PREF_DECIMATION)
            .and_then(|v| v.parse().ok())
            .filter(|d| RATES.iter().any(|(r, _)| r == d))
            .unwrap_or(10);
        let mut errors = Vec::new();
        for path in &entry.paths {
            // the PPK2 ignores the baud rate (USB CDC)
            let (mut reader, mut writer) = match ports::open_serial(path, 115_200) {
                Ok(rw) => rw,
                Err(e) => {
                    errors.push(e);
                    continue;
                }
            };
            let meta = match protocol::handshake(reader.as_mut(), writer.as_mut(), Duration::from_secs(2)) {
                Ok(m) => m,
                Err(e) => {
                    errors.push(format!("No answer from a PPK2 on {path} ({e})."));
                    continue;
                }
            };
            let live = Arc::new(Live {
                vdd_mv: AtomicU32::new(meta.vdd.unwrap_or(3300)),
                decimation: AtomicU32::new(decimation),
            });
            let link = Link::spawn(reader, Box::new(Ppk2Decoder::new(meta.clone(), live.clone())), ctx.clone());
            // like Nordic's app: keep the mode and voltage stored in the PPK2. Ampere meter
            // needs the pass-through on; the source-meter output starts off until the user
            // switches it on (or presses Record).
            // After a reconnect: continue exactly as before.
            let source_meter = meta.mode == Some(2);
            let vdd = meta.vdd.unwrap_or(3300).clamp(VDD_MIN_MV, VDD_MAX_MV);
            let restored = opts.restore.as_deref().and_then(State::decode);
            let state = restored.unwrap_or(State { source_meter, dut_power: !source_meter, vdd_mv: vdd });
            let (source_meter, vdd) = (state.source_meter, state.vdd_mv);
            let mut dev = Ppk2Device {
                entry: entry.clone(),
                port: path.clone(),
                link,
                writer,
                live,
                meta: meta.clone(),
                state,
                decimation,
                events: Vec::new(),
                vdd_edit: vdd as f64 / 1000.0,
                pending_rate: None,
            };
            dev.apply(state);
            let cal =
                if matches!(meta.calibrated.as_deref(), Some("1" | "true")) { "calibrated" } else { "factory values" };
            dev.log(
                Level::Good,
                format!(
                    "PPK2 hardware {}, {cal} · {} · {}",
                    dev.meta.hw.as_deref().unwrap_or("?"),
                    if source_meter { "source meter" } else { "ampere meter" },
                    fmt::si(vdd as f64 / 1000.0, "V"),
                ),
            );
            if source_meter && !state.dut_power {
                dev.log(Level::Warn, "Device power is OFF — switch it on in the PPK2 panel (or just press Record).");
            }
            return Ok(Box::new(dev));
        }
        Err(format!("Could not connect to the PPK2.\n\n{}", errors.join("\n")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::device::ports::PortInfo;

    #[test]
    fn groups_the_two_ports_and_claims_them() {
        let port = |path: &str, serial: &str, interface: u8| PortInfo {
            path: path.into(),
            vid: protocol::VID,
            pid: protocol::PID,
            serial: serial.into(),
            interface,
            ..Default::default()
        };
        let mut sys = SystemPorts {
            ports: vec![
                port("/dev/ttyACM1", "AAA", 3),
                port("/dev/ttyACM0", "AAA", 1),
                PortInfo { path: "/dev/ttyUSB0".into(), ..Default::default() },
            ],
        };
        let mut out = Vec::new();
        Ppk2Driver.discover(&mut sys, &mut out);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].paths, vec!["/dev/ttyACM0", "/dev/ttyACM1"]);
        assert_eq!(sys.unclaimed().count(), 1); // the board driver gets ttyUSB0 only
    }

    #[test]
    fn state_round_trip() {
        let st = State { source_meter: true, dut_power: false, vdd_mv: 1800 };
        assert_eq!(State::decode(&st.encode()), Some(st));
    }
}
