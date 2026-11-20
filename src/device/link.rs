//! The data path from a transport to the GUI.
//!
//! A [`Link`] owns a background thread that reads bytes from a [`ByteReader`],
//! hands them to a [`Decoder`] and sends the results to the GUI in batches
//! (about 30 per second, not one message per frame — that keeps the GUI fast).

use std::io;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, channel};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

/// Reading half of a transport.
pub trait ByteReader: Send {
    /// Read whatever arrived. Must not block for long (≈ 20 ms at most);
    /// `Ok(0)` means "nothing yet". An error ends the link (device gone).
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize>;
}

/// A transport: reading and writing half (e.g. a serial port).
pub type Transport = (Box<dyn ByteReader>, Box<dyn ByteWriter>);

/// Writing half of a transport.
pub trait ByteWriter: Send {
    fn write(&mut self, data: &[u8]) -> io::Result<()>;
}

/// Output of a [`Decoder`] for one chunk of bytes.
#[derive(Default, Debug)]
pub struct Decoded {
    /// Current samples in µA, in order.
    pub samples: Vec<f64>,
    /// Text lines from the device (shown in the device log).
    pub lines: Vec<String>,
    /// Units received (frames, samples, …) and how many of them were damaged or lost.
    pub received: u64,
    pub damaged: u64,
}

/// Turns the byte stream of one protocol into samples. Runs on the link thread.
pub trait Decoder: Send {
    /// Process a chunk of bytes. Chunks can split frames anywhere.
    fn feed(&mut self, bytes: &[u8], out: &mut Decoded);
    /// Called when a recording starts (reset filters, counters, …).
    fn restart(&mut self) {}
}

/// What the link thread sends to the GUI.
#[derive(Debug)]
pub enum Msg {
    Data(Vec<f64>),
    Line(String),
    /// The transport failed (e.g. USB unplugged); the link has stopped.
    Error(String),
}

#[derive(Default)]
struct Shared {
    received: AtomicU64,
    damaged: AtomicU64,
    restart: AtomicBool,
    stop: AtomicBool,
}

pub struct Link {
    rx: Receiver<Msg>,
    shared: Arc<Shared>,
    thread: Option<JoinHandle<()>>,
}

const EMIT_INTERVAL: Duration = Duration::from_millis(30);
const MAX_PENDING: usize = 200_000;

impl Link {
    /// Start the reader thread. `ctx` is used to wake the GUI when data arrives.
    pub fn spawn(mut reader: Box<dyn ByteReader>, mut decoder: Box<dyn Decoder>, ctx: egui::Context) -> Link {
        let (tx, rx) = channel();
        let shared = Arc::new(Shared::default());
        let sh = shared.clone();
        let thread = std::thread::Builder::new()
            .name("device-link".into())
            .spawn(move || {
                let mut buf = vec![0u8; 256 * 1024];
                let mut out = Decoded::default();
                let mut pending: Vec<f64> = Vec::with_capacity(65_536);
                let mut last_emit = Instant::now();
                while !sh.stop.load(Ordering::Relaxed) {
                    if sh.restart.load(Ordering::SeqCst) {
                        decoder.restart();
                        sh.restart.store(false, Ordering::SeqCst);
                    }
                    match reader.read(&mut buf) {
                        Ok(0) => {}
                        Ok(n) => {
                            decoder.feed(&buf[..n], &mut out);
                            sh.received.fetch_add(std::mem::take(&mut out.received), Ordering::Relaxed);
                            sh.damaged.fetch_add(std::mem::take(&mut out.damaged), Ordering::Relaxed);
                            pending.append(&mut out.samples);
                            if !out.lines.is_empty() {
                                for l in out.lines.drain(..) {
                                    let _ = tx.send(Msg::Line(l));
                                }
                                ctx.request_repaint();
                            }
                        }
                        Err(e) => {
                            if !sh.stop.load(Ordering::Relaxed) {
                                let _ = tx.send(Msg::Error(e.to_string()));
                                ctx.request_repaint();
                            }
                            break;
                        }
                    }
                    if !pending.is_empty() && (last_emit.elapsed() >= EMIT_INTERVAL || pending.len() > MAX_PENDING) {
                        let _ = tx.send(Msg::Data(std::mem::take(&mut pending)));
                        last_emit = Instant::now();
                        ctx.request_repaint();
                    }
                }
                if !pending.is_empty() {
                    let _ = tx.send(Msg::Data(pending));
                }
            })
            .expect("could not start the device reader thread");
        Link { rx, shared, thread: Some(thread) }
    }

    /// Next message from the reader thread, if any.
    pub fn try_recv(&self) -> Option<Msg> {
        self.rx.try_recv().ok()
    }

    /// (received, damaged) since the link opened or [`Link::reset_counts`].
    pub fn counts(&self) -> (u64, u64) {
        (self.shared.received.load(Ordering::Relaxed), self.shared.damaged.load(Ordering::Relaxed))
    }

    pub fn reset_counts(&self) {
        self.shared.received.store(0, Ordering::Relaxed);
        self.shared.damaged.store(0, Ordering::Relaxed);
    }

    /// Reset the decoder before a new recording. Waits (briefly) until the reader
    /// thread has done it, so no bytes of the new stream reach the old decoder state
    /// — call it *before* telling the device to start.
    pub fn restart_decoder(&self) {
        self.shared.restart.store(true, Ordering::SeqCst);
        let t0 = Instant::now();
        while self.shared.restart.load(Ordering::SeqCst) && t0.elapsed() < Duration::from_millis(250) {
            if self.thread.as_ref().is_none_or(|t| t.is_finished()) {
                break;
            }
            std::thread::sleep(Duration::from_millis(1));
        }
    }

    /// Stop the reader thread and wait for it.
    pub fn close(&mut self) {
        self.shared.stop.store(true, Ordering::Relaxed);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

impl Drop for Link {
    fn drop(&mut self) {
        self.shared.stop.store(true, Ordering::Relaxed);
    }
}

#[cfg(test)]
pub mod testing {
    //! In-memory transport for tests: what the test pushes into `incoming`
    //! is read by the device; what the device writes ends up in `written`.
    use super::*;
    use std::sync::Mutex;

    #[derive(Clone, Default)]
    pub struct Pipe {
        pub incoming: Arc<Mutex<Vec<u8>>>,
        pub written: Arc<Mutex<Vec<u8>>>,
    }

    impl ByteReader for Pipe {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            let mut inc = self.incoming.lock().unwrap();
            let n = inc.len().min(buf.len());
            buf[..n].copy_from_slice(&inc[..n]);
            inc.drain(..n);
            drop(inc);
            if n == 0 {
                std::thread::sleep(Duration::from_millis(2));
            }
            Ok(n)
        }
    }

    impl ByteWriter for Pipe {
        fn write(&mut self, data: &[u8]) -> io::Result<()> {
            self.written.lock().unwrap().extend_from_slice(data);
            Ok(())
        }
    }

    impl Pipe {
        pub fn push(&self, data: &[u8]) {
            self.incoming.lock().unwrap().extend_from_slice(data);
        }
        pub fn take_written(&self) -> Vec<u8> {
            std::mem::take(&mut *self.written.lock().unwrap())
        }
    }

    /// Collect messages from a link for up to `ms` milliseconds.
    pub fn drain(link: &Link, ms: u64) -> (Vec<f64>, Vec<String>) {
        let (mut s, mut l) = (Vec::new(), Vec::new());
        let t0 = Instant::now();
        while t0.elapsed() < Duration::from_millis(ms) {
            while let Some(m) = link.try_recv() {
                match m {
                    Msg::Data(d) => s.extend(d),
                    Msg::Line(t) => l.push(t),
                    Msg::Error(e) => panic!("link error: {e}"),
                }
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        (s, l)
    }
}
