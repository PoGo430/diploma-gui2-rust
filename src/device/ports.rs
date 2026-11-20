//! Serial-port transport: finding ports and opening them.
//!
//! A USB-bulk (or TCP, BLE, …) transport would be a sibling of this file that
//! also returns a [`ByteReader`] / [`ByteWriter`] pair.

use std::io::{self, Read, Write};
use std::time::Duration;

use super::link::{ByteReader, ByteWriter, Transport};

/// A serial port on this computer.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PortInfo {
    pub path: String,
    /// USB manufacturer + product, or the kernel driver name.
    pub desc: String,
    pub vid: u16,
    pub pid: u16,
    /// USB serial number.
    pub serial: String,
    /// USB interface number (tells several ports of one device apart).
    pub interface: u8,
    /// Taken by a driver during discovery.
    pub claimed: bool,
}

impl PortInfo {
    pub fn is_usb(&self, vid: u16, pid: u16) -> bool {
        self.vid == vid && self.pid == pid
    }
}

/// All serial ports, shared by the drivers during discovery.
pub struct SystemPorts {
    pub ports: Vec<PortInfo>,
}

impl SystemPorts {
    pub fn scan() -> Self {
        let mut ports = list();
        // Extra ports for testing, e.g. virtual ports made with socat:
        //   ANALIZATOR_EXTRA_PORTS  — plain serial ports
        //   ANALIZATOR_PPK2_PORTS   — treated as a PPK2 (USB id 1915:C00A)
        for (var, ppk2) in [("ANALIZATOR_EXTRA_PORTS", false), ("ANALIZATOR_PPK2_PORTS", true)] {
            let Some(extra) = std::env::var_os(var) else { continue };
            for (k, p) in std::env::split_paths(&extra).enumerate() {
                let path = p.to_string_lossy().to_string();
                if p.exists() && !ports.iter().any(|o| o.path == path) {
                    let mut info = PortInfo { path, ..Default::default() };
                    if ppk2 {
                        (info.vid, info.pid) = (super::ppk2::protocol::VID, super::ppk2::protocol::PID);
                        info.serial = "TEST".into();
                        info.interface = k as u8;
                    }
                    ports.push(info);
                }
            }
        }
        ports.sort_by_key(|a| natural_key(&a.path));
        Self { ports }
    }

    /// Mark a port as used by a driver.
    pub fn claim(&mut self, path: &str) {
        if let Some(p) = self.ports.iter_mut().find(|p| p.path == path) {
            p.claimed = true;
        }
    }

    pub fn unclaimed(&self) -> impl Iterator<Item = &PortInfo> {
        self.ports.iter().filter(|p| !p.claimed)
    }
}

/// "COM10" sorts after "COM2", "ttyUSB10" after "ttyUSB2".
fn natural_key(s: &str) -> (String, u64) {
    let t = s.trim_end_matches(|c: char| c.is_ascii_digit());
    (t.to_string(), s[t.len()..].parse().unwrap_or(0))
}

/// Open a serial port as a transport.
pub fn open_serial(path: &str, baud: u32) -> Result<Transport, String> {
    let port =
        serialport::new(path, baud).timeout(Duration::from_millis(20)).open().map_err(|e| open_error(path, &e))?;
    let _ = port.clear(serialport::ClearBuffer::Input);
    let writer = port.try_clone().map_err(|e| format!("Could not open {path}: {e}"))?;
    Ok((Box::new(SerialReader(port)), Box::new(SerialWriter(writer))))
}

fn open_error(path: &str, e: &serialport::Error) -> String {
    let mut msg = format!("Could not open {path}: {e}");
    let text = e.to_string().to_lowercase();
    let denied = matches!(e.kind(), serialport::ErrorKind::Io(io::ErrorKind::PermissionDenied))
        || text.contains("permission")
        || text.contains("access is denied");
    if denied && cfg!(target_os = "windows") {
        msg.push_str(
            "\n\nThe port is probably open in another program (serial monitor, IDE, another Analizator window). Close it and try again.",
        );
    } else if denied {
        msg.push_str(
            "\n\nYour user is probably not allowed to use serial ports. Run this once and log out/in:\n    sudo usermod -aG dialout $USER",
        );
    }
    msg
}

struct SerialReader(Box<dyn serialport::SerialPort>);
struct SerialWriter(Box<dyn serialport::SerialPort>);

impl ByteReader for SerialReader {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        match self.0.read(buf) {
            Err(e)
                if matches!(
                    e.kind(),
                    io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
                ) =>
            {
                Ok(0)
            }
            r => r,
        }
    }
}

impl ByteWriter for SerialWriter {
    fn write(&mut self, data: &[u8]) -> io::Result<()> {
        self.0.write_all(data)?;
        self.0.flush()
    }
}

/// Serial ports with their USB details.
fn list() -> Vec<PortInfo> {
    let mut out = Vec::new();
    #[cfg(target_os = "linux")]
    {
        // read /sys directly (no libudev needed)
        let Ok(rd) = std::fs::read_dir("/sys/class/tty") else { return out };
        for e in rd.flatten() {
            let name = e.file_name().to_string_lossy().to_string();
            let dev = e.path().join("device");
            if !dev.exists() {
                continue;
            }
            let driver = std::fs::read_link(dev.join("driver"))
                .ok()
                .and_then(|p| p.file_name().map(|s| s.to_string_lossy().to_string()))
                .unwrap_or_default();
            let wanted =
                ["ttyUSB", "ttyACM", "ttyAMA", "ttyTHS", "rfcomm", "ttyGS"].iter().any(|p| name.starts_with(p))
                    || (name.starts_with("ttyS") && !matches!(driver.as_str(), "serial8250" | "port" | ""));
            if !wanted {
                continue;
            }
            let mut info = usb_info(&dev).unwrap_or_default();
            if info.desc.is_empty() {
                info.desc = driver;
            }
            info.path = format!("/dev/{name}");
            out.push(info);
        }
    }
    #[cfg(not(target_os = "linux"))]
    {
        if let Ok(ports) = serialport::available_ports() {
            for p in ports {
                let mut info = PortInfo { path: p.port_name.clone(), ..Default::default() };
                match &p.port_type {
                    serialport::SerialPortType::UsbPort(u) => {
                        let (m, pr) =
                            (u.manufacturer.clone().unwrap_or_default(), u.product.clone().unwrap_or_default());
                        let d = if m.is_empty() || pr.starts_with(&m) { pr } else { format!("{m} {pr}") };
                        info.desc = if d.trim().is_empty() {
                            format!("USB {:04x}:{:04x}", u.vid, u.pid)
                        } else {
                            d.trim().to_string()
                        };
                        (info.vid, info.pid) = (u.vid, u.pid);
                        info.serial = u.serial_number.clone().unwrap_or_default();
                    }
                    serialport::SerialPortType::BluetoothPort => info.desc = "Bluetooth".into(),
                    _ => {}
                }
                out.push(info);
            }
        }
    }
    out
}

#[cfg(target_os = "linux")]
fn usb_info(dev: &std::path::Path) -> Option<PortInfo> {
    let read = |p: &std::path::Path, f: &str| std::fs::read_to_string(p.join(f)).ok().map(|s| s.trim().to_string());
    let hex = |s: Option<String>| s.and_then(|v| u16::from_str_radix(&v, 16).ok()).unwrap_or(0);
    let mut p = std::fs::canonicalize(dev).ok()?;
    // `device` points at the USB interface; its number tells several ports of one device apart
    let interface = read(&p, "bInterfaceNumber").and_then(|v| u8::from_str_radix(&v, 16).ok()).unwrap_or(0);
    for _ in 0..5 {
        if let Some(prod) = read(&p, "product") {
            let manu = read(&p, "manufacturer").unwrap_or_default();
            let desc = if manu.is_empty() || prod.starts_with(&manu) { prod } else { format!("{manu} {prod}") };
            return Some(PortInfo {
                desc,
                vid: hex(read(&p, "idVendor")),
                pid: hex(read(&p, "idProduct")),
                serial: read(&p, "serial").unwrap_or_default(),
                interface,
                ..Default::default()
            });
        }
        p = p.parent()?.to_path_buf();
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn natural_order_and_claims() {
        let mut sys = SystemPorts {
            ports: ["COM10", "COM2", "COM1"]
                .iter()
                .map(|p| PortInfo { path: p.to_string(), ..Default::default() })
                .collect(),
        };
        sys.ports.sort_by_key(|a| natural_key(&a.path));
        assert_eq!(sys.ports.iter().map(|p| p.path.as_str()).collect::<Vec<_>>(), ["COM1", "COM2", "COM10"]);
        sys.claim("COM2");
        assert_eq!(sys.unclaimed().map(|p| p.path.as_str()).collect::<Vec<_>>(), ["COM1", "COM10"]);
    }
}
