//! DEMO: a simulated Analizator board.
//!
//! It is a fake *transport* that speaks the real board protocol, so the demo
//! runs through exactly the same decoder and device code as the hardware.

use std::io;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use super::board::{BoardDevice, TextFrameDecoder};
use super::link::{ByteReader, ByteWriter};
use super::ports::SystemPorts;
use super::{Capabilities, Device, DeviceEntry, Driver, OpenOptions};

pub const DEMO_ID: &str = "__demo__";

/// Fake board producing a realistic IoT-style current profile.
pub struct Demo {
    cmd_buf: Vec<u8>,
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
            cmd_buf: Vec::new(),
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

impl ByteReader for DemoReader {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let n = {
            let mut d = self.0.lock().unwrap_or_else(|e| e.into_inner());
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

impl ByteWriter for DemoWriter {
    /// Receives the same bytes a real board would ("START\n" …).
    fn write(&mut self, data: &[u8]) -> io::Result<()> {
        let mut d = self.0.lock().unwrap_or_else(|e| e.into_inner());
        d.cmd_buf.extend_from_slice(data);
        while let Some(nl) = d.cmd_buf.iter().position(|&b| b == b'\n') {
            let line: Vec<u8> = d.cmd_buf.drain(..=nl).collect();
            let cmd = String::from_utf8_lossy(&line).to_string();
            d.command(&cmd);
        }
        Ok(())
    }
}

pub struct DemoDriver;

impl Driver for DemoDriver {
    fn id(&self) -> &'static str {
        "demo"
    }

    fn discover(&self, _sys: &mut SystemPorts, out: &mut Vec<DeviceEntry>) {
        out.push(DeviceEntry {
            driver: self.id(),
            id: DEMO_ID.into(),
            label: "DEMO  —  simulated device".into(),
            detail: "A simulated board with a typical IoT current profile, for trying the app without hardware".into(),
            paths: vec![],
            match_key: String::new(),
        });
    }

    fn open(&self, entry: &DeviceEntry, opts: &OpenOptions, ctx: &egui::Context) -> Result<Box<dyn Device>, String> {
        let sim = Arc::new(Mutex::new(Demo::new(opts.sample_period_s)));
        let sim2 = sim.clone();
        let dev = BoardDevice::new(
            entry.clone(),
            "DEMO".into(),
            Box::new(DemoReader(sim.clone())),
            Box::new(DemoWriter(sim)),
            Box::new(TextFrameDecoder::default()),
            ctx,
        )
        .with_caps(Capabilities { flash: true, ..Default::default() })
        .on_sample_period(move |s| sim2.lock().unwrap_or_else(|e| e.into_inner()).period = s.max(1e-5));
        Ok(Box::new(dev))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::device::link::testing::drain;

    #[test]
    fn demo_streams_after_start() {
        let ctx = egui::Context::default();
        let mut list = Vec::new();
        DemoDriver.discover(&mut SystemPorts { ports: vec![] }, &mut list);
        let opts = OpenOptions { sample_period_s: 1e-3, ..Default::default() };
        let mut dev = DemoDriver.open(&list[0], &opts, &ctx).unwrap();
        let (s, lines) = drain(dev.link(), 50);
        assert!(s.is_empty());
        assert!(lines.contains(&"READY".to_string()));
        dev.start().unwrap();
        let (s, lines) = drain(dev.link(), 300);
        assert!(lines.contains(&"OK START".to_string()));
        assert!(s.len() > 100, "{} samples", s.len()); // ~1 kHz
        dev.close();
    }
}
