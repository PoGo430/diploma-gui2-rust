//! Serial connection, frame parsing and the simulated DEMO board.
//!
//! Board -> PC:  `$DB<v1>,<v2>,...$DE\n`   (current samples in µA)
//!               any other line is shown in the device log
//! PC -> board:  START / STOP / DUMP / ERASE  (newline terminated)

use std::io::{self, Read, Write};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

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
}

/// List serial ports. On Linux this reads /sys directly (no libudev needed).
pub fn list_ports() -> Vec<PortInfo> {
    let mut out: Vec<PortInfo> = Vec::new();
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
                let desc = usb_description(&dev).unwrap_or_else(|| driver.clone());
                let path = format!("/dev/{name}");
                let label = if desc.is_empty() { path.clone() } else { format!("{path}  —  {desc}") };
                out.push(PortInfo { path, label, desc });
            }
        }
    }
    // extra device paths, e.g. a virtual port made with socat (for testing)
    if let Some(extra) = std::env::var_os("ANALIZATOR_EXTRA_PORTS") {
        for p in std::env::split_paths(&extra) {
            let path = p.to_string_lossy().to_string();
            if p.exists() && !out.iter().any(|o| o.path == path) {
                out.push(PortInfo { label: path.clone(), path, desc: String::new() });
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
                out.push(PortInfo { label, path: p.port_name, desc });
            }
        }
    }
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
fn usb_description(dev: &std::path::Path) -> Option<String> {
    let mut p = std::fs::canonicalize(dev).ok()?;
    for _ in 0..5 {
        let product = std::fs::read_to_string(p.join("product")).ok();
        if let Some(prod) = product {
            let manu = std::fs::read_to_string(p.join("manufacturer")).unwrap_or_default();
            let (m, pr) = (manu.trim(), prod.trim());
            return Some(if m.is_empty() || pr.starts_with(m) { pr.to_string() } else { format!("{m} {pr}") });
        }
        p = p.parent()?.to_path_buf();
    }
    None
}

// ------------------------------------------------------------- the link ---

trait CmdWriter: Send {
    fn send(&mut self, cmd: &str) -> io::Result<()>;
}

impl CmdWriter for Box<dyn serialport::SerialPort> {
    fn send(&mut self, cmd: &str) -> io::Result<()> {
        self.write_all(cmd.as_bytes())?;
        self.write_all(b"\n")?;
        self.flush()
    }
}

/// Frame counters, shared with the reader thread.
#[derive(Default)]
pub struct LinkStats {
    pub frames: AtomicU64,
    pub corrupted: AtomicU64,
}

pub struct Connection {
    pub name: String,
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
        Ok(Self { name: path.to_string(), stats, rx, writer: Box::new(writer), stop, thread: Some(thread), demo: None })
    }

    pub fn send(&mut self, cmd: &str) -> Result<(), String> {
        self.writer.send(cmd).map_err(|e| e.to_string())
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
