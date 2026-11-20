//! The Analizator board.
//!
//! Protocol (text, over a serial port):
//!   board → PC   `$DB<v1>,<v2>,...,<vN>$DE\n`   current samples in µA
//!                any other line                 shown in the device log
//!   PC → board   `START`, `STOP`, `DUMP`, `ERASE` + newline
//!
//! To change the protocol (e.g. a binary format for higher sample rates), write
//! a new [`Decoder`] and pass it to [`BoardDevice::new`]; to change the
//! transport (e.g. USB bulk instead of a serial port), pass a different
//! reader/writer pair. Nothing else in the app needs to change.

use super::link::{ByteReader, ByteWriter, Decoded, Decoder, Link};
use super::ports::{self, SystemPorts};
use super::{Capabilities, Device, DeviceEntry, Driver, OpenOptions};

/// Protocol constants of the board firmware.
pub mod protocol {
    pub const FRAME_START: &[u8] = b"$DB";
    pub const FRAME_END: &[u8] = b"$DE";
    pub const SEPARATOR: u8 = b',';
    pub const START: &str = "START";
    pub const STOP: &str = "STOP";
    pub const DUMP: &str = "DUMP";
    pub const ERASE: &str = "ERASE";

    /// Bytes to send for a command.
    pub fn command(cmd: &str) -> Vec<u8> {
        let mut v = cmd.as_bytes().to_vec();
        v.push(b'\n');
        v
    }
}

// ------------------------------------------------------------------ decoder ---

/// What one line of the stream contained.
#[derive(Debug, PartialEq)]
pub enum Line {
    Empty,
    /// A text line (shown in the device log).
    Text(String),
    /// A data frame; `ok` is false if it was damaged (missing start/end marker or a bad value).
    Frame {
        ok: bool,
    },
}

/// Parses one line. Samples go into `out`.
pub fn parse_line(line: &[u8], out: &mut Vec<f64>) -> Line {
    use protocol::*;
    let line = line.trim_ascii();
    if line.is_empty() {
        return Line::Empty;
    }
    let Some(s) = find(line, FRAME_START) else {
        if find(line, FRAME_END).is_some() {
            return Line::Frame { ok: false }; // start of the frame was lost
        }
        return Line::Text(String::from_utf8_lossy(line).into_owned());
    };
    let body = &line[s + FRAME_START.len()..];
    let Some(e) = find(body, FRAME_END) else {
        return Line::Frame { ok: false }; // end of the frame was lost: drop it
    };
    let mut ok = line[..s].trim_ascii().is_empty();
    for field in body[..e].split(|&b| b == SEPARATOR) {
        let f = field.trim_ascii();
        if f.is_empty() {
            continue;
        }
        match std::str::from_utf8(f).ok().and_then(|t| t.parse::<f64>().ok()) {
            Some(v) => out.push(v),
            None => ok = false,
        }
    }
    Line::Frame { ok }
}

fn find(hay: &[u8], needle: &[u8]) -> Option<usize> {
    hay.windows(needle.len()).position(|w| w == needle)
}

/// Decoder for the `$DB…$DE` text frames.
#[derive(Default)]
pub struct TextFrameDecoder {
    acc: Vec<u8>,
    /// The first line may have been cut in half when we connected: don't count it as damaged.
    seen_line: bool,
}

impl Decoder for TextFrameDecoder {
    fn feed(&mut self, bytes: &[u8], out: &mut Decoded) {
        self.acc.extend_from_slice(bytes);
        let Some(nl) = self.acc.iter().rposition(|&b| b == b'\n') else {
            if self.acc.len() > 1_000_000 {
                self.acc.clear(); // garbage without newlines
            }
            return;
        };
        for line in self.acc[..nl].split(|&b| b == b'\n') {
            match parse_line(line, &mut out.samples) {
                Line::Empty => continue,
                Line::Text(t) => out.lines.push(t),
                Line::Frame { ok } => {
                    out.received += 1;
                    if !ok && self.seen_line {
                        out.damaged += 1;
                    }
                }
            }
            self.seen_line = true;
        }
        self.acc.drain(..=nl);
    }
}

// ------------------------------------------------------------------- device ---

pub struct BoardDevice {
    entry: DeviceEntry,
    name: String,
    link: Link,
    writer: Box<dyn ByteWriter>,
    caps: Capabilities,
    on_period: Option<Box<dyn Fn(f64) + Send>>,
}

impl BoardDevice {
    pub fn new(
        entry: DeviceEntry,
        name: String,
        reader: Box<dyn ByteReader>,
        writer: Box<dyn ByteWriter>,
        decoder: Box<dyn Decoder>,
        ctx: &egui::Context,
    ) -> Self {
        let link = Link::spawn(reader, decoder, ctx.clone());
        let caps = Capabilities { flash: true, baud_rate: true, ..Default::default() };
        Self { entry, name, link, writer, caps, on_period: None }
    }

    pub fn with_caps(mut self, caps: Capabilities) -> Self {
        self.caps = caps;
        self
    }

    /// Called when the user changes the sample period (used by the DEMO board).
    pub fn on_sample_period(mut self, f: impl Fn(f64) + Send + 'static) -> Self {
        self.on_period = Some(Box::new(f));
        self
    }

    fn command(&mut self, cmd: &str) -> Result<(), String> {
        self.writer.write(&protocol::command(cmd)).map_err(|e| format!("Could not send {cmd}: {e}"))
    }
}

impl Device for BoardDevice {
    fn name(&self) -> String {
        self.name.clone()
    }
    fn entry(&self) -> &DeviceEntry {
        &self.entry
    }
    fn caps(&self) -> Capabilities {
        self.caps
    }
    fn link(&self) -> &Link {
        &self.link
    }
    fn start(&mut self) -> Result<(), String> {
        self.link.restart_decoder();
        self.command(protocol::START)
    }
    fn stop(&mut self) -> Result<(), String> {
        self.command(protocol::STOP)
    }
    fn dump_flash(&mut self) -> Result<(), String> {
        self.command(protocol::DUMP)
    }
    fn erase_flash(&mut self) -> Result<(), String> {
        self.command(protocol::ERASE)
    }
    fn set_sample_period(&mut self, seconds: f64) {
        if let Some(f) = &self.on_period {
            f(seconds);
        }
    }
    fn close(mut self: Box<Self>) {
        self.link.close();
    }
}

// ------------------------------------------------------------------- driver ---

/// Any serial port not claimed by another driver is offered as an Analizator board.
pub struct BoardDriver;

impl Driver for BoardDriver {
    fn id(&self) -> &'static str {
        "board"
    }

    fn uses_baud_rate(&self) -> bool {
        true
    }

    fn discover(&self, sys: &mut SystemPorts, out: &mut Vec<DeviceEntry>) {
        for p in sys.unclaimed() {
            let label = if p.desc.is_empty() { p.path.clone() } else { format!("{}  —  {}", p.path, p.desc) };
            let usb = p.vid != 0 || !p.serial.is_empty();
            let match_key =
                if usb { format!("usb:{:04x}:{:04x}:{}:{}", p.vid, p.pid, p.serial, p.desc) } else { String::new() };
            out.push(DeviceEntry {
                driver: self.id(),
                id: p.path.clone(),
                label,
                detail: if usb { format!("USB {:04x}:{:04x} {}", p.vid, p.pid, p.serial) } else { p.desc.clone() },
                paths: vec![p.path.clone()],
                match_key,
            });
        }
    }

    fn open(&self, entry: &DeviceEntry, opts: &OpenOptions, ctx: &egui::Context) -> Result<Box<dyn Device>, String> {
        let path = entry.paths.first().cloned().unwrap_or_else(|| entry.id.clone());
        let (reader, writer) = ports::open_serial(&path, opts.baud)?;
        Ok(Box::new(BoardDevice::new(entry.clone(), path, reader, writer, Box::new(TextFrameDecoder::default()), ctx)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::device::link::testing::{Pipe, drain};

    #[test]
    fn parses_frames() {
        let mut v = Vec::new();
        assert_eq!(parse_line(b"$DB1.5, 2,3.25,$DE\r", &mut v), Line::Frame { ok: true });
        assert_eq!(v, vec![1.5, 2.0, 3.25]);
        assert_eq!(parse_line(b"hello board", &mut v), Line::Text("hello board".into()));
        assert_eq!(parse_line(b"$DB1,2", &mut v), Line::Frame { ok: false }); // end lost -> dropped
        assert_eq!(parse_line(b"5,6$DE", &mut v), Line::Frame { ok: false }); // start lost
        assert_eq!(v.len(), 3);
        assert_eq!(parse_line(b"$DB7,x8,9$DE", &mut v), Line::Frame { ok: false }); // bad value
        assert_eq!(v, vec![1.5, 2.0, 3.25, 7.0, 9.0]);
        assert_eq!(parse_line(b"  ", &mut v), Line::Empty);
    }

    #[test]
    fn decoder_handles_split_chunks() {
        let mut d = TextFrameDecoder::default();
        let mut out = Decoded::default();
        for chunk in [&b"READY\n$DB1,2"[..], b",3$DE\n$D", b"B4$DE\ngarb$DE\n"] {
            d.feed(chunk, &mut out);
        }
        assert_eq!(out.samples, vec![1.0, 2.0, 3.0, 4.0]);
        assert_eq!(out.lines, vec!["READY".to_string()]);
        assert_eq!((out.received, out.damaged), (3, 1));
    }

    /// The whole device over an in-memory transport: commands out, samples in.
    #[test]
    fn device_round_trip() {
        let pipe = Pipe::default();
        let ctx = egui::Context::default();
        let entry = DeviceEntry {
            driver: "board",
            id: "test".into(),
            label: String::new(),
            detail: String::new(),
            paths: vec![],
            match_key: String::new(),
        };
        let mut dev = BoardDevice::new(
            entry,
            "test".into(),
            Box::new(pipe.clone()),
            Box::new(pipe.clone()),
            Box::new(TextFrameDecoder::default()),
            &ctx,
        );
        dev.start().unwrap();
        assert_eq!(pipe.take_written(), b"START\n");
        pipe.push(b"OK START\n$DB10,20,30$DE\n");
        let (samples, lines) = drain(dev.link(), 150);
        assert_eq!(samples, vec![10.0, 20.0, 30.0]);
        assert_eq!(lines, vec!["OK START".to_string()]);
        assert_eq!(dev.link().counts(), (1, 0));
        dev.erase_flash().unwrap();
        assert_eq!(pipe.take_written(), b"ERASE\n");
        Box::new(dev).close();
    }
}
