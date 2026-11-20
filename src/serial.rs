//! Serial connection, frame parsing and the simulated DEMO board.
//!
//! Board -> PC:  `$DB<v1>,<v2>,...$DE\n`   (current samples in µA)
//!               any other line is shown in the device log
//! PC -> board:  START / STOP / DUMP / ERASE  (newline terminated)

use std::io::{self, Read, Write};
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use crate::ppk2;

pub const DEMO_PORT: &str = "__demo__";

pub enum Msg {
    Data(Vec<f64>),
    Line(String),
    Error(String),
}

// ---------------------------------------------------------------- ports ---

#[derive(Clone, Debug, PartialEq)]
pub struct PortInfo {
    pub path: String,
    pub label: String,
    /// USB product name etc. (used to find the device again after it was re-plugged)
    pub desc: String,
    /// Nordic Power Profiler Kit II
    pub ppk2: bool,
    /// Other serial ports of the same USB device (the PPK2 has two; one of them carries the data).
    pub alt_paths: Vec<String>,
}

/// USB details of a port (Linux: from /sys).
#[derive(Default, Clone)]
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
struct UsbInfo {
    desc: String,
    vid: u16,
    pid: u16,
    serial: String,
    interface: u8,
}

const PPK2_LABEL: &str = "PPK2  —  Nordic Power Profiler Kit II";

/// Collapse the PPK2's two serial ports into one entry ("first" port by USB
/// interface number, like Nordic's own app), keeping the other as a fallback.
fn merge_ppk2(raw: Vec<(PortInfo, UsbInfo)>) -> Vec<PortInfo> {
    let mut out: Vec<PortInfo> = Vec::new();
    let mut ppk: Vec<(PortInfo, UsbInfo)> = Vec::new();
    for (p, u) in raw {
        if p.ppk2 { ppk.push((p, u)) } else { out.push(p) }
    }
    ppk.sort_by(|a, b| (a.1.serial.clone(), a.1.interface, a.0.path.clone()).cmp(&(b.1.serial.clone(), b.1.interface, b.0.path.clone())));
    let mut i = 0;
    while i < ppk.len() {
        let serial = ppk[i].1.serial.clone();
        let mut j = i + 1;
        while j < ppk.len() && ppk[j].1.serial == serial {
            j += 1;
        }
        let mut first = ppk[i].0.clone();
        first.alt_paths = ppk[i + 1..j].iter().map(|(p, _)| p.path.clone()).collect();
        let short = if serial.is_empty() { String::new() } else { format!(" ({})", &serial[..serial.len().min(8)]) };
        first.label = format!("PPK2{short}  —  {}", first.path);
        first.desc = PPK2_LABEL.to_string();
        out.push(first);
        i = j;
    }
    out
}

/// List serial ports. On Linux this reads /sys directly (no libudev needed).
pub fn list_ports() -> Vec<PortInfo> {
    let mut raw: Vec<(PortInfo, UsbInfo)> = Vec::new();
    #[cfg(target_os = "linux")]
    {
        if let Ok(rd) = std::fs::read_dir("/sys/class/tty") {
            for e in rd.flatten() {
                let name = e.file_name().to_string_lossy().to_string();
                let sys = e.path();
                let dev = sys.join("device");
                if !dev.exists() {
                    continue;
                }
                let driver = std::fs::read_link(dev.join("driver"))
                    .ok()
                    .and_then(|p| p.file_name().map(|s| s.to_string_lossy().to_string()))
                    .unwrap_or_default();
                let wanted = ["ttyUSB", "ttyACM", "ttyAMA", "ttyTHS", "rfcomm", "ttyGS"].iter().any(|p| name.starts_with(p))
                    || (name.starts_with("ttyS") && !matches!(driver.as_str(), "serial8250" | "port" | ""));
                if !wanted {
                    continue;
                }
                let usb = usb_info(&dev).unwrap_or_default();
                let desc = if usb.desc.is_empty() { driver.clone() } else { usb.desc.clone() };
                let path = format!("/dev/{name}");
                let label = if desc.is_empty() { path.clone() } else { format!("{path}  —  {desc}") };
                let is_ppk2 = usb.vid == ppk2::VID && usb.pid == ppk2::PID;
                raw.push((PortInfo { path, label, desc, ppk2: is_ppk2, alt_paths: vec![] }, usb));
            }
        }
    }
    // extra device paths, e.g. a virtual port made with socat (for testing);
    // ANALIZATOR_PPK2_PORTS adds ports that are treated as a PPK2
    for (var, is_ppk2) in [("ANALIZATOR_EXTRA_PORTS", false), ("ANALIZATOR_PPK2_PORTS", true)] {
        if let Some(extra) = std::env::var_os(var) {
            for (k, p) in std::env::split_paths(&extra).enumerate() {
                let path = p.to_string_lossy().to_string();
                if p.exists() && !raw.iter().any(|o| o.0.path == path) {
                    let usb = UsbInfo { serial: if is_ppk2 { "TEST".into() } else { String::new() }, interface: k as u8, ..Default::default() };
                    raw.push((PortInfo { label: path.clone(), path, desc: String::new(), ppk2: is_ppk2, alt_paths: vec![] }, usb));
                }
            }
        }
    }
    #[cfg(not(target_os = "linux"))]
    {
        // Windows / macOS: e.g. "COM3  —  STMicroelectronics Virtual COM Port"
        if let Ok(ports) = serialport::available_ports() {
            for p in ports {
                let desc = match &p.port_type {
                    serialport::SerialPortType::UsbPort(u) => {
                        let (m, pr) = (u.manufacturer.clone().unwrap_or_default(), u.product.clone().unwrap_or_default());
                        let d = if m.is_empty() || pr.starts_with(&m) { pr } else { format!("{m} {pr}") };
                        if d.trim().is_empty() { format!("USB {:04x}:{:04x}", u.vid, u.pid) } else { d.trim().to_string() }
                    }
                    serialport::SerialPortType::BluetoothPort => "Bluetooth".to_string(),
                    _ => String::new(),
                };
                let label = if desc.is_empty() { p.port_name.clone() } else { format!("{}  —  {desc}", p.port_name) };
                let (is_ppk2, usb) = match &p.port_type {
                    serialport::SerialPortType::UsbPort(u) => (
                        u.vid == ppk2::VID && u.pid == ppk2::PID,
                        UsbInfo { desc: desc.clone(), vid: u.vid, pid: u.pid, serial: u.serial_number.clone().unwrap_or_default(), interface: 0 },
                    ),
                    _ => (false, UsbInfo::default()),
                };
                raw.push((PortInfo { label, path: p.port_name, desc, ppk2: is_ppk2, alt_paths: vec![] }, usb));
            }
        }
    }
    let mut out = merge_ppk2(raw);
    // natural order: COM2 before COM10, ttyUSB2 before ttyUSB10
    out.sort_by(|a, b| {
        let split = |s: &str| {
            let t = s.trim_end_matches(|c: char| c.is_ascii_digit());
            (t.to_string(), s[t.len()..].parse::<u64>().unwrap_or(0))
        };
        split(&a.path).cmp(&split(&b.path))
    });
    out
}

#[cfg(target_os = "linux")]
fn usb_info(dev: &std::path::Path) -> Option<UsbInfo> {
    let read = |p: &std::path::Path, f: &str| std::fs::read_to_string(p.join(f)).ok().map(|s| s.trim().to_string());
    let hex = |s: Option<String>| s.and_then(|v| u16::from_str_radix(&v, 16).ok()).unwrap_or(0);
    let mut p = std::fs::canonicalize(dev).ok()?;
    // `device` points at the USB interface; its number tells the PPK2's two ports apart
    let interface = read(&p, "bInterfaceNumber").and_then(|v| u8::from_str_radix(&v, 16).ok()).unwrap_or(0);
    for _ in 0..5 {
        if let Some(prod) = read(&p, "product") {
            let manu = read(&p, "manufacturer").unwrap_or_default();
            let desc = if manu.is_empty() || prod.starts_with(&manu) { prod } else { format!("{manu} {prod}") };
            return Some(UsbInfo {
                desc,
                vid: hex(read(&p, "idVendor")),
                pid: hex(read(&p, "idProduct")),
                serial: read(&p, "serial").unwrap_or_default(),
                interface,
            });
        }
        p = p.parent()?.to_path_buf();
    }
    None
}

// ------------------------------------------------------------- the link ---

trait CmdWriter: Send {
    fn send(&mut self, cmd: &str) -> io::Result<()>;
    fn send_raw(&mut self, _bytes: &[u8]) -> io::Result<()> {
        Ok(())
    }
}

impl CmdWriter for Box<dyn serialport::SerialPort> {
    fn send(&mut self, cmd: &str) -> io::Result<()> {
        self.write_all(cmd.as_bytes())?;
        self.write_all(b"\n")?;
        self.flush()
    }
    fn send_raw(&mut self, bytes: &[u8]) -> io::Result<()> {
        self.write_all(bytes)?;
        self.flush()
    }
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum LinkKind {
    Board,
    Demo,
    Ppk2,
}

/// PPK2 settings the reader thread needs (changed from the GUI while running).
pub struct Ppk2Shared {
    vdd_mv: AtomicU32,
    decimation: AtomicU32,
    restart: AtomicBool,
}

/// Frame counters, shared with the reader thread.
#[derive(Default)]
pub struct LinkStats {
    pub frames: AtomicU64,
    pub corrupted: AtomicU64,
}

pub struct Connection {
    pub name: String,
    pub kind: LinkKind,
    /// Calibration data read from the PPK2 when connecting.
    pub ppk2_meta: Option<ppk2::Meta>,
    ppk2: Option<Arc<Ppk2Shared>>,
    pub stats: Arc<LinkStats>,
    pub rx: Receiver<Msg>,
    writer: Box<dyn CmdWriter>,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
    demo: Option<Arc<Mutex<Demo>>>,
}

impl Connection {
    pub fn open(path: &str, baud: u32, period_s: f64, ctx: egui::Context) -> Result<Self, String> {
        let (tx, rx) = channel();
        let stop = Arc::new(AtomicBool::new(false));
        let stats = Arc::new(LinkStats::default());
        if path == DEMO_PORT {
            let demo = Arc::new(Mutex::new(Demo::new(period_s)));
            let reader = DemoReader(demo.clone());
            let thread = spawn_reader(reader, tx, stop.clone(), stats.clone(), ctx);
            return Ok(Self {
                name: "DEMO".into(),
                kind: LinkKind::Demo,
                ppk2_meta: None,
                ppk2: None,
                stats,
                rx,
                writer: Box::new(DemoWriter(demo.clone())),
                stop,
                thread: Some(thread),
                demo: Some(demo),
            });
        }
        let port = serialport::new(path, baud)
            .timeout(Duration::from_millis(20))
            .open()
            .map_err(|e| {
                let mut msg = format!("Could not open {path}: {e}");
                let denied = matches!(e.kind(), serialport::ErrorKind::Io(io::ErrorKind::PermissionDenied))
                    || e.to_string().to_lowercase().contains("permission")
                    || e.to_string().to_lowercase().contains("access is denied");
                if denied && cfg!(target_os = "windows") {
                    msg.push_str("\n\nThe port is probably open in another program (serial monitor, IDE, another Analizator window). Close it and try again.");
                } else if denied {
                    msg.push_str(
                        "\n\nYour user is probably not allowed to use serial ports. Run this once and log out/in:\n    sudo usermod -aG dialout $USER",
                    );
                }
                msg
            })?;
        let _ = port.clear(serialport::ClearBuffer::Input);
        let writer = port.try_clone().map_err(|e| format!("Could not open {path}: {e}"))?;
        let thread = spawn_reader(port, tx, stop.clone(), stats.clone(), ctx);
        Ok(Self { name: path.to_string(), kind: LinkKind::Board, ppk2_meta: None, ppk2: None, stats, rx, writer: Box::new(writer), stop, thread: Some(thread), demo: None })
    }

    /// Open a PPK2. Tries `paths` in order (the PPK2 has two serial ports) and uses
    /// the first one that answers the metadata request.
    pub fn open_ppk2(paths: &[String], decimation: u32, ctx: egui::Context) -> Result<Self, String> {
        let mut errors = Vec::new();
        for path in paths {
            match ppk2_handshake(path) {
                Ok((port, meta)) => {
                    let (tx, rx) = channel();
                    let stop = Arc::new(AtomicBool::new(false));
                    let stats = Arc::new(LinkStats::default());
                    let shared = Arc::new(Ppk2Shared {
                        vdd_mv: AtomicU32::new(meta.vdd.unwrap_or(3300)),
                        decimation: AtomicU32::new(decimation.max(1)),
                        restart: AtomicBool::new(true),
                    });
                    let writer = port.try_clone().map_err(|e| format!("Could not open {path}: {e}"))?;
                    let thread = spawn_ppk2_reader(port, meta.clone(), shared.clone(), tx, stop.clone(), stats.clone(), ctx);
                    return Ok(Self {
                        name: path.clone(),
                        kind: LinkKind::Ppk2,
                        ppk2_meta: Some(meta),
                        ppk2: Some(shared),
                        stats,
                        rx,
                        writer: Box::new(writer),
                        stop,
                        thread: Some(thread),
                        demo: None,
                    });
                }
                Err(e) => errors.push(e),
            }
        }
        Err(errors.join("\n\n"))
    }

    pub fn send(&mut self, cmd: &str) -> Result<(), String> {
        if self.kind == LinkKind::Ppk2 {
            // translate the board's text commands into PPK2 byte commands
            return match cmd {
                "START" => {
                    if let Some(sh) = &self.ppk2 {
                        sh.restart.store(true, Ordering::Relaxed);
                    }
                    self.send_raw(&[ppk2::cmd::AVERAGE_START])
                }
                "STOP" => self.send_raw(&[ppk2::cmd::AVERAGE_STOP]),
                other => Err(format!("{other} is not supported by the PPK2")),
            };
        }
        self.writer.send(cmd).map_err(|e| e.to_string())
    }

    pub fn send_raw(&mut self, bytes: &[u8]) -> Result<(), String> {
        self.writer.send_raw(bytes).map_err(|e| e.to_string())
    }

    /// PPK2: set the supply voltage (also used by the current calculation).
    pub fn ppk2_set_vdd(&mut self, mv: u32) -> Result<(), String> {
        let mv = mv.clamp(ppk2::VDD_MIN_MV, ppk2::VDD_MAX_MV);
        if let Some(sh) = &self.ppk2 {
            sh.vdd_mv.store(mv, Ordering::Relaxed);
        }
        self.send_raw(&ppk2::regulator_cmd(mv))
    }

    /// PPK2: average this many 10 µs samples into one (1 = 100 kHz, 100 = 1 kHz).
    pub fn ppk2_set_decimation(&self, n: u32) {
        if let Some(sh) = &self.ppk2 {
            sh.decimation.store(n.max(1), Ordering::Relaxed);
        }
    }

    /// (frames received, damaged frames) since connecting or the last reset.
    pub fn frame_counts(&self) -> (u64, u64) {
        (self.stats.frames.load(Ordering::Relaxed), self.stats.corrupted.load(Ordering::Relaxed))
    }

    pub fn reset_frame_counts(&self) {
        self.stats.frames.store(0, Ordering::Relaxed);
        self.stats.corrupted.store(0, Ordering::Relaxed);
    }

    pub fn set_demo_period(&self, period_s: f64) {
        if let Some(d) = &self.demo {
            d.lock().unwrap().period = period_s.max(1e-5);
        }
    }

    pub fn close(mut self) {
        if self.kind == LinkKind::Ppk2 {
            let _ = self.send_raw(&[ppk2::cmd::AVERAGE_STOP]);
        }
        self.stop.store(true, Ordering::Relaxed);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

impl Drop for Connection {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

fn spawn_reader<R: Read + Send + 'static>(
    mut port: R,
    tx: Sender<Msg>,
    stop: Arc<AtomicBool>,
    stats: Arc<LinkStats>,
    ctx: egui::Context,
) -> JoinHandle<()> {
    std::thread::Builder::new()
        .name("serial-reader".into())
        .spawn(move || {
            let mut buf = vec![0u8; 64 * 1024];
            let mut acc: Vec<u8> = Vec::with_capacity(256 * 1024);
            let mut pending: Vec<f64> = Vec::with_capacity(65536);
            let mut last_emit = Instant::now();
                    // the first line may have been cut in half when we connected: don't count it as damaged
            let mut first_line = true;
            while !stop.load(Ordering::Relaxed) {
                match port.read(&mut buf) {
                    Ok(0) => {}
                    Ok(n) => {
                        acc.extend_from_slice(&buf[..n]);
                        if let Some(nl) = acc.iter().rposition(|&b| b == b'\n') {
                            for line in acc[..nl].split(|&b| b == b'\n') {
                                match parse_line(line, &mut pending) {
                                    Parsed::Empty => continue,
                                    Parsed::Text(text) => {
                                        let _ = tx.send(Msg::Line(text));
                                        ctx.request_repaint();
                                    }
                                    Parsed::Frame { ok } => {
                                        stats.frames.fetch_add(1, Ordering::Relaxed);
                                        if !ok && !first_line {
                                            stats.corrupted.fetch_add(1, Ordering::Relaxed);
                                        }
                                    }
                                }
                                first_line = false;
                            }
                            acc.drain(..=nl);
                        } else if acc.len() > 1_000_000 {
                            acc.clear(); // garbage without newlines
                        }
                    }
                    Err(e) if matches!(e.kind(), io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted) => {}
                    Err(e) => {
                        if !stop.load(Ordering::Relaxed) {
                            let _ = tx.send(Msg::Error(e.to_string()));
                            ctx.request_repaint();
                        }
                        break;
                    }
                }
                // hand data to the GUI in batches (~30 per second), not line by line
                if !pending.is_empty() && (last_emit.elapsed() >= Duration::from_millis(30) || pending.len() > 200_000) {
                    let _ = tx.send(Msg::Data(std::mem::take(&mut pending)));
                    last_emit = Instant::now();
                    ctx.request_repaint();
                }
            }
            if !pending.is_empty() {
                let _ = tx.send(Msg::Data(pending));
            }
        })
        .expect("spawn reader thread")
}

/// Open a PPK2 port, stop any running measurement and read its calibration data.
fn ppk2_handshake(path: &str) -> Result<(Box<dyn serialport::SerialPort>, ppk2::Meta), String> {
    let mut port = serialport::new(path, 115_200)
        .timeout(Duration::from_millis(20))
        .open()
        .map_err(|e| format!("Could not open {path}: {e}"))?;
    let _ = port.write_all(&[ppk2::cmd::AVERAGE_STOP]);
    let _ = port.flush();
    // throw away whatever was still streaming
    let mut buf = [0u8; 65536];
    let t0 = Instant::now();
    let mut quiet_since = Instant::now();
    while t0.elapsed() < Duration::from_millis(1500) && quiet_since.elapsed() < Duration::from_millis(120) {
        if let Ok(n) = port.read(&mut buf) {
            if n > 0 {
                quiet_since = Instant::now();
            }
        }
    }
    port.write_all(&[ppk2::cmd::GET_META_DATA]).map_err(|e| format!("Could not write to {path}: {e}"))?;
    let _ = port.flush();
    let mut text = Vec::new();
    let t0 = Instant::now();
    while t0.elapsed() < Duration::from_secs(2) {
        if let Ok(n) = port.read(&mut buf) {
            text.extend_from_slice(&buf[..n]);
            if text.windows(3).any(|w| w == b"END") {
                let meta = ppk2::Meta::parse(&String::from_utf8_lossy(&text));
                return Ok((port, meta));
            }
        }
    }
    Err(format!("No answer from a PPK2 on {path} (no calibration data received)."))
}

fn spawn_ppk2_reader(
    mut port: Box<dyn serialport::SerialPort>,
    meta: ppk2::Meta,
    shared: Arc<Ppk2Shared>,
    tx: Sender<Msg>,
    stop: Arc<AtomicBool>,
    stats: Arc<LinkStats>,
    ctx: egui::Context,
) -> JoinHandle<()> {
    std::thread::Builder::new()
        .name("ppk2-reader".into())
        .spawn(move || {
            let mut stream = ppk2::Stream::new(meta);
            let mut buf = vec![0u8; 256 * 1024];
            let mut pending: Vec<f64> = Vec::with_capacity(65536);
            let mut last_emit = Instant::now();
            while !stop.load(Ordering::Relaxed) {
                if shared.restart.swap(false, Ordering::Relaxed) {
                    stream.reset();
                }
                match port.read(&mut buf) {
                    Ok(0) => {}
                    Ok(n) => {
                        let vdd = shared.vdd_mv.load(Ordering::Relaxed) as f64;
                        let dec = shared.decimation.load(Ordering::Relaxed);
                        let (r0, l0) = (stream.received, stream.lost);
                        stream.feed(&buf[..n], vdd, dec, &mut pending);
                        stats.frames.fetch_add(stream.received - r0, Ordering::Relaxed);
                        stats.corrupted.fetch_add(stream.lost - l0, Ordering::Relaxed);
                    }
                    Err(e) if matches!(e.kind(), io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted) => {}
                    Err(e) => {
                        if !stop.load(Ordering::Relaxed) {
                            let _ = tx.send(Msg::Error(e.to_string()));
                            ctx.request_repaint();
                        }
                        break;
                    }
                }
                if !pending.is_empty() && (last_emit.elapsed() >= Duration::from_millis(30) || pending.len() > 200_000) {
                    let _ = tx.send(Msg::Data(std::mem::take(&mut pending)));
                    last_emit = Instant::now();
                    ctx.request_repaint();
                }
            }
            if !pending.is_empty() {
                let _ = tx.send(Msg::Data(pending));
            }
        })
        .expect("spawn PPK2 reader thread")
}

#[derive(Debug, PartialEq)]
pub enum Parsed {
    Empty,
    /// A text line from the board (shown in the device log).
    Text(String),
    /// A data frame; `ok` is false if it was damaged (missing start/end marker
    /// or a value that is not a number).
    Frame { ok: bool },
}

/// Parses one line. Samples go into `out`.
pub fn parse_line(line: &[u8], out: &mut Vec<f64>) -> Parsed {
    let line = line.trim_ascii();
    if line.is_empty() {
        return Parsed::Empty;
    }
    let Some(s) = find(line, b"$DB") else {
        if find(line, b"$DE").is_some() {
            return Parsed::Frame { ok: false }; // start of the frame was lost
        }
        return Parsed::Text(String::from_utf8_lossy(line).into_owned());
    };
    let body = &line[s + 3..];
    let Some(e) = find(body, b"$DE") else {
        return Parsed::Frame { ok: false }; // end of the frame was lost: drop it
    };
    let mut ok = s == 0 || line[..s].trim_ascii().is_empty();
    for field in body[..e].split(|&b| b == b',') {
        let f = field.trim_ascii();
        if f.is_empty() {
            continue;
        }
        match std::str::from_utf8(f).ok().and_then(|t| t.parse::<f64>().ok()) {
            Some(v) => out.push(v),
            None => ok = false,
        }
    }
    Parsed::Frame { ok }
}

fn find(hay: &[u8], needle: &[u8]) -> Option<usize> {
    hay.windows(needle.len()).position(|w| w == needle)
}

// ------------------------------------------------------------ demo board ---

/// Fake board producing a realistic IoT-style current profile.
pub struct Demo {
    out: Vec<u8>,
    running: bool,
    t_last: Instant,
    k: u64,
    pub period: f64,
    rng: u64,
}

impl Demo {
    fn new(period: f64) -> Self {
        Self {
            out: b"Analizator demo board v1.0\nREADY\n".to_vec(),
            running: false,
            t_last: Instant::now(),
            k: 0,
            period: period.max(1e-5),
            rng: 0x9E3779B97F4A7C15,
        }
    }

    fn rand(&mut self) -> f64 {
        // xorshift64*
        self.rng ^= self.rng >> 12;
        self.rng ^= self.rng << 25;
        self.rng ^= self.rng >> 27;
        (self.rng.wrapping_mul(0x2545F4914F6CDD1D) >> 11) as f64 / (1u64 << 53) as f64
    }

    fn gauss(&mut self) -> f64 {
        let u1 = self.rand().max(1e-12);
        let u2 = self.rand();
        (-2.0 * u1.ln()).sqrt() * (std::f64::consts::TAU * u2).cos()
    }

    fn sample(&mut self) -> f64 {
        let t = self.k as f64 * self.period;
        self.k += 1;
        let mut y = 6.0 + 1.5 * (std::f64::consts::TAU * 0.3 * t).sin() + 0.6 * self.gauss(); // sleep ~6 µA
        let ph = t % 2.0; // radio event every 2 s
        if ph > 0.100 && ph < 0.160 {
            y += 9500.0 + 250.0 * self.gauss(); // TX
        } else if ph > 0.160 && ph < 0.230 {
            y += 4200.0 + 120.0 * self.gauss(); // RX
        }
        if t % 0.25 < 0.004 {
            y += 1800.0 + 60.0 * self.gauss(); // CPU wake-ups
        }
        y.max(0.0)
    }

    fn emit(&mut self, n: usize) {
        use std::fmt::Write as _;
        let mut line = String::with_capacity(64 * 12);
        let mut left = n;
        while left > 0 {
            let m = left.min(64);
            line.clear();
            line.push_str("$DB");
            for j in 0..m {
                if j > 0 {
                    line.push(',');
                }
                let v = self.sample();
                let _ = write!(line, "{v:.3}");
            }
            line.push_str("$DE\n");
            self.out.extend_from_slice(line.as_bytes());
            left -= m;
        }
    }

    fn generate(&mut self) {
        if !self.running {
            return;
        }
        let n = (self.t_last.elapsed().as_secs_f64() / self.period) as u64;
        if n > 0 {
            self.t_last += Duration::from_secs_f64(n as f64 * self.period);
            self.emit(n.min(200_000) as usize);
        }
    }

    fn command(&mut self, cmd: &str) {
        match cmd.trim().to_ascii_uppercase().as_str() {
            "START" => {
                self.running = true;
                self.t_last = Instant::now();
                self.out.extend_from_slice(b"OK START\n");
            }
            "STOP" => {
                self.running = false;
                self.out.extend_from_slice(b"OK STOP\n");
            }
            "DUMP" => {
                self.out.extend_from_slice(b"DUMP BEGIN\n");
                let saved = self.k;
                self.k = 0;
                self.emit(30_000);
                self.k = saved;
                self.out.extend_from_slice(b"DUMP END\n");
            }
            "ERASE" => self.out.extend_from_slice(b"Flash erased\n"),
            other => self.out.extend_from_slice(format!("Unknown command: {other}\n").as_bytes()),
        }
    }
}

struct DemoReader(Arc<Mutex<Demo>>);
struct DemoWriter(Arc<Mutex<Demo>>);

impl Read for DemoReader {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let n = {
            let mut d = self.0.lock().unwrap();
            d.generate();
            let n = d.out.len().min(buf.len());
            buf[..n].copy_from_slice(&d.out[..n]);
            d.out.drain(..n);
            n
        };
        if n == 0 {
            std::thread::sleep(Duration::from_millis(10));
        }
        Ok(n)
    }
}

impl CmdWriter for DemoWriter {
    fn send(&mut self, cmd: &str) -> io::Result<()> {
        self.0.lock().unwrap().command(cmd);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_frames() {
        let mut v = Vec::new();
        assert_eq!(parse_line(b"$DB1.5, 2,3.25,$DE\r", &mut v), Parsed::Frame { ok: true });
        assert_eq!(v, vec![1.5, 2.0, 3.25]);
        assert_eq!(parse_line(b"hello board", &mut v), Parsed::Text("hello board".into()));
        assert_eq!(parse_line(b"$DB1,2", &mut v), Parsed::Frame { ok: false }); // end lost -> dropped
        assert_eq!(parse_line(b"5,6$DE", &mut v), Parsed::Frame { ok: false }); // start lost
        assert_eq!(v.len(), 3);
        assert_eq!(parse_line(b"$DB7,x8,9$DE", &mut v), Parsed::Frame { ok: false }); // bad value
        assert_eq!(v, vec![1.5, 2.0, 3.25, 7.0, 9.0]);
        assert_eq!(parse_line(b"  ", &mut v), Parsed::Empty);
    }
}
