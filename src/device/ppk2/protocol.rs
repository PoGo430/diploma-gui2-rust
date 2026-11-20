//! Nordic Power Profiler Kit II (PPK2) protocol: commands, calibration data and
//! decoding of the sample stream. No GUI or port handling here.
//!
//! Protocol details follow Nordic's own nRF Connect Power Profiler app
//! (github.com/NordicSemiconductor/pc-nrfconnect-ppk, src/device/serialDevice.ts)
//! and the IRNAS ppk2-api-python driver.
//!
//! * USB CDC serial port, VID 0x1915 / PID 0xC00A.
//! * Commands are raw bytes (no newline), e.g. `[0x06]` = start sampling.
//! * `GetMetadata` (0x19) answers with text lines `key: value`, ending in `END`.
//!   They hold per-range calibration values and the stored supply voltage/mode.
//! * While sampling, the PPK2 streams 100 000 samples/s, 4 bytes each, little endian:
//!   bits 0-13 ADC value, 14-16 current range (0-4), 18-23 sample counter,
//!   24-31 digital inputs D0-D7.

pub const VID: u16 = 0x1915;
pub const PID: u16 = 0xC00A;
pub const SAMPLE_RATE: f64 = 100_000.0;
pub const VDD_MIN_MV: u32 = 800;
pub const VDD_MAX_MV: u32 = 5000;

pub mod cmd {
    pub const AVERAGE_START: u8 = 0x06;
    pub const AVERAGE_STOP: u8 = 0x07;
    pub const DEVICE_RUNNING_SET: u8 = 0x0c;
    pub const REGULATOR_SET: u8 = 0x0d;
    pub const SET_POWER_MODE: u8 = 0x11;
    pub const GET_META_DATA: u8 = 0x19;
}

/// `[0x0d, high byte, low byte]` of the voltage in mV.
pub fn regulator_cmd(mv: u32) -> [u8; 3] {
    let mv = mv.clamp(VDD_MIN_MV, VDD_MAX_MV);
    [cmd::REGULATOR_SET, (mv >> 8) as u8, (mv & 0xff) as u8]
}

/// Source meter = the PPK2 powers the device; ampere meter = it measures a device on its own supply.
pub fn power_mode_cmd(source_meter: bool) -> [u8; 2] {
    [cmd::SET_POWER_MODE, if source_meter { 2 } else { 1 }]
}

pub fn dut_power_cmd(on: bool) -> [u8; 2] {
    [cmd::DEVICE_RUNNING_SET, on as u8]
}

/// Calibration values and settings reported by the PPK2.
#[derive(Clone, Debug, PartialEq)]
pub struct Meta {
    pub r: [f64; 5],
    pub gs: [f64; 5],
    pub gi: [f64; 5],
    pub o: [f64; 5],
    pub s: [f64; 5],
    pub i: [f64; 5],
    pub ug: [f64; 5],
    /// Supply voltage stored in the PPK2 (mV).
    pub vdd: Option<u32>,
    /// 1 = ampere meter, 2 = source meter.
    pub mode: Option<u8>,
    pub hw: Option<String>,
    pub calibrated: Option<String>,
}

impl Default for Meta {
    fn default() -> Self {
        Self {
            r: [1031.64, 101.65, 10.15, 0.94, 0.043],
            gs: [1.0; 5],
            gi: [1.0; 5],
            o: [0.0; 5],
            s: [0.0; 5],
            i: [0.0; 5],
            ug: [1.0; 5],
            vdd: None,
            mode: None,
            hw: None,
            calibrated: None,
        }
    }
}

impl Meta {
    /// Parses the text answer to `GetMetadata`. Like Nordic's app, values that are
    /// missing, zero or not a number keep their defaults.
    pub fn parse(text: &str) -> Meta {
        let mut m = Meta::default();
        for line in text.lines() {
            let Some((k, v)) = line.split_once(':') else { continue };
            let key = k.trim().to_ascii_lowercase();
            let val = v.trim();
            let num = val.parse::<f64>().ok().filter(|x| x.is_finite() && *x != 0.0);
            match key.as_str() {
                "vdd" => m.vdd = num.map(|x| x as u32),
                "mode" => m.mode = num.map(|x| x as u8),
                "hw" => m.hw = Some(val.to_string()),
                "calibrated" => m.calibrated = Some(val.to_string()),
                _ => {
                    let (name, idx) = key.split_at(key.len().saturating_sub(1));
                    let Ok(idx) = idx.parse::<usize>() else { continue };
                    if idx >= 5 {
                        continue;
                    }
                    let arr = match name {
                        "r" => &mut m.r,
                        "gs" => &mut m.gs,
                        "gi" => &mut m.gi,
                        "o" => &mut m.o,
                        "s" => &mut m.s,
                        "i" => &mut m.i,
                        "ug" => &mut m.ug,
                        _ => continue,
                    };
                    if let Some(x) = num {
                        arr[idx] = x;
                    }
                }
            }
        }
        m
    }
}

/// Turns raw ADC words into µA (calibration formula + Nordic's spike filter).
pub struct Converter {
    pub meta: Meta,
    rolling_avg: Option<f64>,
    rolling_avg4: Option<f64>,
    prev_range: Option<usize>,
    consecutive_range_samples: u32,
    after_spike: i32,
}

const ADC_MULT: f64 = 1.8 / 163_840.0;
const SPIKE_ALPHA: f64 = 0.18;
const SPIKE_ALPHA5: f64 = 0.06;
const SPIKE_SAMPLES: i32 = 3;

impl Converter {
    pub fn new(meta: Meta) -> Self {
        Self {
            meta,
            rolling_avg: None,
            rolling_avg4: None,
            prev_range: None,
            consecutive_range_samples: 0,
            after_spike: 0,
        }
    }

    /// Call when sampling (re)starts.
    pub fn reset(&mut self) {
        self.rolling_avg = None;
        self.rolling_avg4 = None;
        self.prev_range = None;
        self.consecutive_range_samples = 0;
        self.after_spike = 0;
    }

    /// Current in amperes, exactly as Nordic's `getAdcResult`.
    pub fn convert(&mut self, range: usize, adc_val: f64, vdd_mv: f64) -> f64 {
        let m = &self.meta;
        let without_gain = (adc_val - m.o[range]) * (ADC_MULT / m.r[range]);
        let mut adc = m.ug[range]
            * (without_gain * (m.gs[range] * without_gain + m.gi[range])
                + (m.s[range] * (vdd_mv / 1000.0) + m.i[range]));

        let prev_avg = self.rolling_avg;
        let prev_avg4 = self.rolling_avg4;
        let avg = match self.rolling_avg {
            None => adc,
            Some(r) => SPIKE_ALPHA * adc + (1.0 - SPIKE_ALPHA) * r,
        };
        let avg4 = match self.rolling_avg4 {
            None => adc,
            Some(r) => SPIKE_ALPHA5 * adc + (1.0 - SPIKE_ALPHA5) * r,
        };
        self.rolling_avg = Some(avg);
        self.rolling_avg4 = Some(avg4);
        let prev_range = *self.prev_range.get_or_insert(range);

        // after a range switch the PPK2 output spikes; smooth the next few samples
        if prev_range != range || self.after_spike > 0 {
            if prev_range != range {
                self.consecutive_range_samples = 0;
                self.after_spike = SPIKE_SAMPLES;
            } else {
                self.consecutive_range_samples += 1;
            }
            if range == 4 {
                if self.consecutive_range_samples < 2 {
                    self.rolling_avg = prev_avg;
                    self.rolling_avg4 = prev_avg4;
                }
                adc = self.rolling_avg4.unwrap_or(adc);
            } else {
                adc = self.rolling_avg.unwrap_or(adc);
            }
            self.after_spike -= 1;
        }
        self.prev_range = Some(range);
        adc
    }
}

/// Splits the byte stream into 4-byte samples, converts them, checks the sample
/// counter for lost samples and averages groups of `decimation` samples.
pub struct Stream {
    pub conv: Converter,
    remainder: Vec<u8>,
    expected_counter: Option<u32>,
    last_value: f64,
    acc_sum: f64,
    acc_n: u32,
    /// Raw samples received / lost (counter gaps).
    pub received: u64,
    pub lost: u64,
}

impl Stream {
    pub fn new(meta: Meta) -> Self {
        Self {
            conv: Converter::new(meta),
            remainder: Vec::new(),
            expected_counter: None,
            last_value: 0.0,
            acc_sum: 0.0,
            acc_n: 0,
            received: 0,
            lost: 0,
        }
    }

    /// Call when sampling (re)starts.
    pub fn reset(&mut self) {
        self.conv.reset();
        self.expected_counter = None;
        self.acc_sum = 0.0;
        self.acc_n = 0;
    }

    fn push_value(&mut self, ua: f64, decimation: u32, out: &mut Vec<f64>) {
        self.acc_sum += ua;
        self.acc_n += 1;
        if self.acc_n >= decimation.max(1) {
            out.push(self.acc_sum / self.acc_n as f64);
            self.acc_sum = 0.0;
            self.acc_n = 0;
        }
    }

    /// Feed bytes from the port; finished samples (µA) are appended to `out`.
    pub fn feed(&mut self, bytes: &[u8], vdd_mv: f64, decimation: u32, out: &mut Vec<f64>) {
        let mut data = std::mem::take(&mut self.remainder);
        data.extend_from_slice(bytes);
        let (words, rest) = data.as_chunks::<4>();
        for w in words {
            let word = u32::from_le_bytes(*w);
            let adc = (word & 0x3fff) as f64 * 4.0;
            let range = (((word >> 14) & 0x7) as usize).min(4);
            let counter = (word >> 18) & 0x3f;
            let ua = self.conv.convert(range, adc, vdd_mv) * 1e6;
            self.received += 1;
            // the 6-bit counter goes up by one per sample; a jump means samples were lost
            if let Some(exp) = self.expected_counter
                && counter != exp
            {
                let missing = (counter.wrapping_sub(exp)) & 0x3f;
                self.lost += missing as u64;
                // keep the time axis right: repeat the last value for the missing samples
                for _ in 0..missing {
                    let v = self.last_value;
                    self.push_value(v, decimation, out);
                }
            }
            self.expected_counter = Some((counter + 1) & 0x3f);
            self.last_value = ua;
            self.push_value(ua, decimation, out);
        }
        self.remainder = rest.to_vec();
    }
}

// ------------------------------------------------------------- link glue ---

use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, Instant};

use crate::device::link::{ByteReader, ByteWriter, Decoded, Decoder};

/// Settings the GUI changes while the reader thread runs.
#[derive(Debug)]
pub struct Live {
    pub vdd_mv: AtomicU32,
    pub decimation: AtomicU32,
}

/// [`Decoder`] for the PPK2 sample stream.
pub struct Ppk2Decoder {
    stream: Stream,
    live: Arc<Live>,
}

impl Ppk2Decoder {
    pub fn new(meta: Meta, live: Arc<Live>) -> Self {
        Self { stream: Stream::new(meta), live }
    }
}

impl Decoder for Ppk2Decoder {
    fn feed(&mut self, bytes: &[u8], out: &mut Decoded) {
        let (r0, l0) = (self.stream.received, self.stream.lost);
        let vdd = self.live.vdd_mv.load(Ordering::Relaxed) as f64;
        let dec = self.live.decimation.load(Ordering::Relaxed);
        self.stream.feed(bytes, vdd, dec, &mut out.samples);
        out.received += self.stream.received - r0;
        out.damaged += self.stream.lost - l0;
    }

    fn restart(&mut self) {
        self.stream.reset();
    }
}

/// Stop any running measurement and read the calibration data.
pub fn handshake(reader: &mut dyn ByteReader, writer: &mut dyn ByteWriter, timeout: Duration) -> Result<Meta, String> {
    let _ = writer.write(&[cmd::AVERAGE_STOP]);
    // throw away whatever was still streaming
    let mut buf = vec![0u8; 65536];
    let t0 = Instant::now();
    let mut quiet_since = Instant::now();
    while t0.elapsed() < Duration::from_millis(1500) && quiet_since.elapsed() < Duration::from_millis(120) {
        if let Ok(n) = reader.read(&mut buf)
            && n > 0
        {
            quiet_since = Instant::now();
        }
    }
    writer.write(&[cmd::GET_META_DATA]).map_err(|e| format!("Could not write: {e}"))?;
    let mut text = Vec::new();
    let t0 = Instant::now();
    while t0.elapsed() < timeout {
        match reader.read(&mut buf) {
            Ok(n) => {
                text.extend_from_slice(&buf[..n]);
                if text.windows(3).any(|w| w == b"END") {
                    return Ok(Meta::parse(&String::from_utf8_lossy(&text)));
                }
            }
            Err(e) => return Err(format!("Read error: {e}")),
        }
    }
    Err("no calibration data received".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    const META_TEXT: &str = "Calibrated: 0\nR0: 1031.64\nR1: 101.65\nR2: 10.15\nR3: 0.94\nR4: 0.043\n\
        GS0: 0.0001\nGS1: 0.0002\nGS2: 0.0003\nGS3: 0.0004\nGS4: 0.0005\n\
        GI0: 1.01\nGI1: 1.02\nGI2: 1.03\nGI3: 1.04\nGI4: 1.05\n\
        O0: 112.5\nO1: 74.1\nO2: 60.0\nO3: 52.0\nO4: 50.0\n\
        S0: 0\nS1: 0\nS2: 0\nS3: 0\nS4: -nan\nI0: 0\nI1: 0\nI2: 0\nI3: 0\nI4: 0\n\
        UG0: 1\nUG1: 1\nUG2: 1\nUG3: 1\nUG4: 1\nVDD: 3300\nHW: 13234\nmode: 2\nIA: 56\nEND\n";

    #[test]
    fn parses_metadata() {
        let m = Meta::parse(META_TEXT);
        assert_eq!(m.r[4], 0.043);
        assert_eq!(m.gs[2], 0.0003);
        assert_eq!(m.gi[4], 1.05);
        assert_eq!(m.o[0], 112.5);
        assert_eq!(m.s[4], 0.0); // "-nan" keeps the default
        assert_eq!(m.vdd, Some(3300));
        assert_eq!(m.mode, Some(2));
        assert_eq!(m.hw.as_deref(), Some("13234"));
    }

    #[test]
    fn regulator_matches_python_driver() {
        // ppk2-api-python's encoding: b1 = 3 + (mV - 800 + 32) / 256, b2 = (mV - 800 + 32) % 256
        for mv in [800u32, 1800, 3300, 3700, 5000] {
            let d = mv - 800 + 32;
            assert_eq!(regulator_cmd(mv), [0x0d, (3 + d / 256) as u8, (d % 256) as u8]);
        }
        assert_eq!(regulator_cmd(100), regulator_cmd(800));
    }

    /// The formula against a hand calculation (no spike filtering on the first sample).
    #[test]
    fn conversion_formula() {
        let m = Meta::parse(META_TEXT);
        let mut c = Converter::new(m.clone());
        let adc = 5000.0 * 4.0;
        let got = c.convert(2, adc, 3300.0);
        let wg = (adc - m.o[2]) * (1.8 / 163840.0 / m.r[2]);
        let want = m.ug[2] * (wg * (m.gs[2] * wg + m.gi[2]) + (m.s[2] * 3.3 + m.i[2]));
        assert!((got - want).abs() < 1e-15);
        // range 2 (10.15 Ω), ADC 20000, offset 60, gain 1.03 -> 19940 * 1.8/163840/10.15 * 1.03 ≈ 22.23 mA
        assert!((got * 1e3 - 22.23).abs() < 0.01, "{got}");
    }

    fn word(adc: u32, range: u32, counter: u32) -> [u8; 4] {
        (adc | (range << 14) | ((counter & 0x3f) << 18)).to_le_bytes()
    }

    #[test]
    fn stream_split_lost_and_decimation() {
        let mut s = Stream::new(Meta::default());
        let mut bytes = Vec::new();
        for k in 0..10u32 {
            if k == 6 || k == 7 {
                continue; // two samples lost
            }
            bytes.extend_from_slice(&word(1000, 1, k));
        }
        let mut out = Vec::new();
        // feed in awkward pieces to exercise the remainder handling
        for piece in bytes.chunks(3) {
            s.feed(piece, 3300.0, 1, &mut out);
        }
        assert_eq!(s.received, 8);
        assert_eq!(s.lost, 2);
        assert_eq!(out.len(), 10); // gaps filled, time axis intact
        assert!(out.iter().all(|v| (v - out[0]).abs() < 1e-9));
        // decimation by 5 -> 2 averaged values
        let mut s = Stream::new(Meta::default());
        let mut out = Vec::new();
        let b: Vec<u8> = (0..10u32).flat_map(|k| word(1000, 1, k)).collect();
        s.feed(&b, 3300.0, 5, &mut out);
        assert_eq!(out.len(), 2);
    }

    #[test]
    fn handshake_reads_metadata() {
        use crate::device::link::testing::Pipe;
        let pipe = Pipe::default();
        let (mut r, mut w) = (pipe.clone(), pipe.clone());
        // the "device" answers once it was asked
        let p2 = pipe.clone();
        let answer = std::thread::spawn(move || {
            for _ in 0..200 {
                if p2.written.lock().unwrap().contains(&cmd::GET_META_DATA) {
                    p2.push(META_TEXT.as_bytes());
                    return;
                }
                std::thread::sleep(Duration::from_millis(5));
            }
        });
        let m = handshake(&mut r, &mut w, Duration::from_secs(2)).unwrap();
        answer.join().unwrap();
        assert_eq!(m.vdd, Some(3300));
        assert_eq!(pipe.take_written(), vec![cmd::AVERAGE_STOP, cmd::GET_META_DATA]);
        // a silent port times out
        let silent = Pipe::default();
        let (mut r, mut w) = (silent.clone(), silent.clone());
        assert!(handshake(&mut r, &mut w, Duration::from_millis(200)).is_err());
    }

    #[test]
    fn counter_wraps_without_loss() {
        let mut s = Stream::new(Meta::default());
        let b: Vec<u8> = (0..200u32).flat_map(|k| word(500, 0, k)).collect();
        let mut out = Vec::new();
        s.feed(&b, 3300.0, 1, &mut out);
        assert_eq!((s.received, s.lost, out.len()), (200, 0, 200));
    }
}
