//! Measurement devices.
//!
//! Everything device-specific lives in this module; the GUI only talks to the
//! [`Device`] trait. A device is built from three independent layers:
//!
//! ```text
//!   transport        protocol decoder         device
//!   (bytes in/out)   (bytes → samples)        (behaviour, commands, settings panel)
//!   ports.rs  ─────▶ board::TextFrameDecoder  board::BoardDevice
//!   serial port      ppk2::protocol           ppk2::Ppk2Device
//!   demo.rs (fake)                            …
//! ```
//!
//! * A **transport** is just a [`link::ByteReader`] + [`link::ByteWriter`] pair.
//!   Today that is a serial port ([`ports::open_serial`]) or the built-in DEMO
//!   simulator. A USB bulk-endpoint transport would be one more pair.
//! * A **decoder** ([`link::Decoder`]) turns the incoming bytes into current
//!   samples (µA) and text lines. It runs on a background thread owned by a
//!   [`link::Link`], which batches the samples for the GUI.
//! * A **device** ([`Device`]) knows the commands (start, stop, …), what it can
//!   do ([`Capabilities`]) and may draw its own settings panel.
//! * A **driver** ([`Driver`]) finds devices of its kind and opens them. All
//!   drivers are listed in [`DRIVERS`].
//!
//! See DEVELOPING.md for a step-by-step guide to adding a device or moving the
//! board to another transport.

pub mod board;
pub mod demo;
pub mod link;
pub mod ports;
pub mod ppk2;

use std::collections::BTreeMap;

pub use link::{Link, Msg};

/// Every driver the app knows, in priority order: a driver listed earlier gets
/// first pick of the serial ports (the PPK2 driver must claim its ports before
/// the generic board driver takes all remaining ones).
pub static DRIVERS: &[&dyn Driver] = &[&ppk2::Ppk2Driver, &board::BoardDriver, &demo::DemoDriver];

/// One entry of the device list.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct DeviceEntry {
    /// Id of the [`Driver`] that found it.
    pub driver: &'static str,
    /// Unique, stable id (e.g. the port path); remembered as the last used device.
    pub id: String,
    /// Shown in the device list.
    pub label: String,
    /// Tooltip (USB product name, serial number, …).
    pub detail: String,
    /// Transport addresses to try, in order (e.g. serial port paths).
    pub paths: Vec<String>,
    /// Finds the device again after it was unplugged, even under a new port name
    /// (USB serial number or product name). Empty = only the same path counts.
    pub match_key: String,
}

/// Settings the GUI hands to a driver when opening a device.
#[derive(Clone, Debug, Default)]
pub struct OpenOptions {
    pub baud: u32,
    /// Sample period chosen by the user (s), for devices that do not fix it themselves.
    pub sample_period_s: f64,
    /// Remembered per-device preferences ("ppk2.decimation" → "10"), see [`DeviceEvent::SetPref`].
    pub prefs: BTreeMap<String, String>,
    /// When reopening after a lost connection: what [`Device::save_state`] returned,
    /// so the device starts in the same state (mode, voltage, …).
    pub restore: Option<String>,
}

/// What a device can do. The GUI enables buttons and fields from this.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Capabilities {
    /// Has on-board flash: "Dump flash" / "Erase flash".
    pub flash: bool,
    /// Uses the baud-rate setting.
    pub baud_rate: bool,
    /// Sample period fixed by the device (s). `None`: the user sets it.
    pub fixed_sample_period: Option<f64>,
    /// Supply voltage of the device under test, when the device sets it (V).
    pub supply_voltage: Option<f64>,
}

/// Severity of a log message; the GUI picks the colour.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Level {
    Info,
    Good,
    Warn,
    Error,
}

/// Things a device wants the GUI to do (collected with [`Device::take_events`]).
#[derive(Clone, Debug, PartialEq)]
pub enum DeviceEvent {
    Log(Level, String),
    /// Throw away the samples shown (e.g. the sample rate changed).
    ClearData,
    /// Remember a preference for the next time the device is opened.
    SetPref(String, String),
}

/// GUI state a settings panel may need.
pub struct PanelCtx {
    pub recording: bool,
    pub has_data: bool,
}

/// An open device. Created by [`Driver::open`], used only from the GUI thread
/// (but `Send` so it can be opened on a background thread).
pub trait Device: Send {
    /// Where it is connected, e.g. "/dev/ttyACM0" or "DEMO".
    fn name(&self) -> String;
    /// Short name for the status badge, e.g. "PPK2".
    fn badge(&self) -> String {
        self.name()
    }
    /// The list entry it was opened from (used to find it again after a reconnect).
    fn entry(&self) -> &DeviceEntry;
    fn caps(&self) -> Capabilities;
    /// Incoming samples, text lines and errors, plus counters.
    fn link(&self) -> &Link;

    /// Start streaming samples.
    fn start(&mut self) -> Result<(), String>;
    /// Stop streaming samples.
    fn stop(&mut self) -> Result<(), String>;
    fn dump_flash(&mut self) -> Result<(), String> {
        Err("This device has no flash memory.".into())
    }
    fn erase_flash(&mut self) -> Result<(), String> {
        Err("This device has no flash memory.".into())
    }
    /// The user changed the sample period (only for devices without a fixed one).
    fn set_sample_period(&mut self, _seconds: f64) {}

    /// Names for the status bar counters: (received, damaged).
    fn counter_names(&self) -> (&'static str, &'static str) {
        ("frames", "corrupted")
    }
    /// Tooltip for the "damaged" counter.
    fn counter_help(&self) -> &'static str {
        "Data frames that arrived damaged (missing start/end marker or a bad value) since the recording started"
    }

    /// Title of the settings panel, if the device has one.
    fn panel_title(&self) -> Option<&'static str> {
        None
    }
    /// Draw the device's own settings panel.
    fn panel_ui(&mut self, _ui: &mut egui::Ui, _ctx: &PanelCtx) {}
    /// Events produced since the last call (log lines, preference changes, …).
    fn take_events(&mut self) -> Vec<DeviceEvent> {
        Vec::new()
    }

    /// Settings to restore after a reconnect; handed back in [`OpenOptions::restore`].
    fn save_state(&self) -> Option<String> {
        None
    }

    /// Stop the device and its reader thread.
    fn close(self: Box<Self>);
}

/// Finds and opens one kind of device.
pub trait Driver: Sync {
    /// Short stable id, stored in [`DeviceEntry::driver`].
    fn id(&self) -> &'static str;
    /// Add the devices of this kind to `out`. Claim the serial ports you use
    /// with [`ports::SystemPorts::claim`] so later drivers skip them.
    fn discover(&self, sys: &mut ports::SystemPorts, out: &mut Vec<DeviceEntry>);
    /// Whether the baud-rate setting applies (shown before connecting).
    fn uses_baud_rate(&self) -> bool {
        false
    }
    /// Open a device found by [`Driver::discover`]. May block for a few seconds
    /// (it runs on a background thread).
    fn open(&self, entry: &DeviceEntry, opts: &OpenOptions, ctx: &egui::Context) -> Result<Box<dyn Device>, String>;
}

/// The current device list.
pub fn discover() -> Vec<DeviceEntry> {
    let mut sys = ports::SystemPorts::scan();
    let mut out = Vec::new();
    for d in DRIVERS {
        d.discover(&mut sys, &mut out);
    }
    out
}

pub fn driver(id: &str) -> Option<&'static dyn Driver> {
    DRIVERS.iter().copied().find(|d| d.id() == id)
}

pub fn open(entry: &DeviceEntry, opts: &OpenOptions, ctx: &egui::Context) -> Result<Box<dyn Device>, String> {
    let d = driver(entry.driver).ok_or_else(|| format!("Unknown device type '{}'", entry.driver))?;
    d.open(entry, opts, ctx)
}

/// Look for a device that disappeared: the same id first, then (if it came back
/// under another port name) the same USB device via its match key.
pub fn find_again(lost: &DeviceEntry, list: &[DeviceEntry]) -> Option<DeviceEntry> {
    let same_driver = || list.iter().filter(|e| e.driver == lost.driver);
    same_driver()
        .find(|e| e.id == lost.id)
        .or_else(|| same_driver().find(|e| !lost.match_key.is_empty() && e.match_key == lost.match_key))
        .cloned()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(driver: &'static str, id: &str, key: &str) -> DeviceEntry {
        DeviceEntry { driver, id: id.into(), match_key: key.into(), ..Default::default() }
    }

    #[test]
    fn finds_device_again() {
        let lost = entry("board", "/dev/ttyACM0", "usb:STM32 VCP:123");
        // same path
        let list = vec![entry("board", "/dev/ttyACM0", "other")];
        assert_eq!(find_again(&lost, &list).unwrap().id, "/dev/ttyACM0");
        // came back as ttyACM1
        let list = vec![entry("board", "/dev/ttyUSB0", "x"), entry("board", "/dev/ttyACM1", "usb:STM32 VCP:123")];
        assert_eq!(find_again(&lost, &list).unwrap().id, "/dev/ttyACM1");
        // other driver does not count
        let list = vec![entry("ppk2", "/dev/ttyACM0", "usb:STM32 VCP:123")];
        assert!(find_again(&lost, &list).is_none());
        // no match key: only the same path
        let lost = entry("board", "/dev/ttyS1", "");
        assert!(find_again(&lost, &[entry("board", "/dev/ttyS2", "")]).is_none());
    }

    #[test]
    fn drivers_have_unique_ids() {
        let mut ids: Vec<_> = DRIVERS.iter().map(|d| d.id()).collect();
        ids.sort();
        ids.dedup();
        assert_eq!(ids.len(), DRIVERS.len());
    }
}
