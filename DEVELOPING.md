# Developing Analizator

## Building and checking

```sh
cargo run                      # debug build
cargo build --release          # fast build: target/release/analizator
cargo test                     # unit tests (devices are tested over in-memory transports)
cargo clippy --all-targets     # lints (CI fails on warnings)
cargo fmt                      # formatting (rustfmt.toml: 120 columns)
```

See the README for portable Linux builds (glibc 2.27+) and the Windows cross-build.

## Architecture

```
src/
  main.rs            window setup, crash handler (writes crash_*.txt into the logs folder)
  app/               the GUI — only talks to devices through the `Device` trait
    mod.rs           state, settings, per-frame loop
    connection.rs    device list, connect (background thread), reconnect, recording
    files.rs         CSV open/save/export, drag & drop, logs folder
    topbar.rs        toolbar, status bar, keyboard shortcuts
    sidebar.rs       measurement, filter, statistics, battery & sleep, device log
    plots.rs         current/power plots, overview strip
    markers.rs       markers
    dialogs.rs       confirmations, errors, About
  ui/                colours (theme.rs) and small widgets shared with device panels
  device/            everything device-specific
    mod.rs           `Device` + `Driver` traits, `DRIVERS` list, discovery
    link.rs          `ByteReader`/`ByteWriter` (transport), `Decoder`, `Link` (reader thread)
    ports.rs         serial transport: listing ports, opening them
    board.rs         the Analizator board: protocol constants, `TextFrameDecoder`, device, driver
    demo.rs          simulated board (a fake transport that speaks the board protocol)
    ppk2/            Nordic PPK2: protocol.rs (bytes ↔ samples, no GUI) + mod.rs (device, panel, driver)
  data.rs            sample storage with per-256-sample min/max/sum blocks
  filter.rs          moving average, median, low-pass
  csvio.rs           capture file format
  fmt.rs             number/time formatting
```

### The data path

```
 transport              Link thread                                GUI thread
 ByteReader ──bytes──▶  Decoder::feed ──samples (µA), text lines──▶ Msg::Data / Msg::Line
 (serial, demo, …)      batches every ~30 ms, wakes the GUI         → DataStore, device log
 ByteWriter ◀──────────────────────────── commands ───────────────  Device::start / stop / …
```

A device is three independent pieces:

| Piece | Trait | Job | Examples |
|---|---|---|---|
| Transport | `ByteReader` + `ByteWriter` | move bytes | serial port (`ports::open_serial`), DEMO simulator, in-memory `Pipe` in tests |
| Decoder | `Decoder` | bytes → samples in µA (+ text lines, counters) | `board::TextFrameDecoder`, `ppk2::protocol::Ppk2Decoder` |
| Device | `Device` | commands, capabilities, optional settings panel | `board::BoardDevice`, `ppk2::Ppk2Device` |

A **driver** (`Driver`) finds devices of one kind and opens them. `device::DRIVERS` lists all
drivers; the GUI shows whatever they discover.

The GUI never knows which device it is talking to. It asks the device for:

* `caps()` — has flash? uses the baud rate? fixed sample period? sets the supply voltage?
  Buttons and fields are enabled from this.
* `panel_title()` / `panel_ui()` — the device's own sidebar section (the PPK2 panel).
* `take_events()` — log lines, "clear the data", preferences to remember (`SetPref`).
* `counter_names()` — labels for the status-bar counters ("frames / corrupted", "samples / lost").
* `save_state()` — handed back via `OpenOptions::restore` when reopening after a reconnect.

## Recipe: change the board protocol

Everything about the board's protocol is at the top of `src/device/board.rs`:

* `protocol` module: frame markers, separator, command strings.
* `TextFrameDecoder`: parses the frames.

Small changes (other markers, other command names) are edits to the `protocol` module.

For a new format (e.g. **binary frames** for higher sample rates), write a new decoder next to the
text one and use it in `BoardDriver::open`:

```rust
/// Binary frames: [0xA5, n, n × i32 little-endian (nA), checksum]
#[derive(Default)]
pub struct BinaryFrameDecoder { acc: Vec<u8> }

impl Decoder for BinaryFrameDecoder {
    fn feed(&mut self, bytes: &[u8], out: &mut Decoded) {
        self.acc.extend_from_slice(bytes);
        // find complete frames in self.acc, check the checksum,
        // push each value / 1000.0 (µA) into out.samples,
        // count frames in out.received and bad ones in out.damaged,
        // keep an incomplete frame at the end of self.acc for the next call
    }
    fn restart(&mut self) { self.acc.clear(); }
}
```

```rust
// BoardDriver::open
Ok(Box::new(BoardDevice::new(entry.clone(), path, reader, writer, Box::new(BinaryFrameDecoder::default()), ctx)))
```

Add a unit test that feeds the decoder bytes split at awkward places (see
`decoder_handles_split_chunks` in board.rs). The GUI, files, statistics and plots need no change.

## Recipe: move the board to USB (instead of a serial port)

A transport is just a reader and a writer. For native USB (e.g. bulk endpoints with the
[`rusb`](https://crates.io/crates/rusb) crate) add `src/device/usb.rs`:

```rust
pub struct UsbReader { handle: Arc<rusb::DeviceHandle<rusb::GlobalContext>>, ep_in: u8 }
pub struct UsbWriter { handle: Arc<rusb::DeviceHandle<rusb::GlobalContext>>, ep_out: u8 }

impl ByteReader for UsbReader {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        match self.handle.read_bulk(self.ep_in, buf, Duration::from_millis(20)) {
            Ok(n) => Ok(n),
            Err(rusb::Error::Timeout) => Ok(0),               // nothing yet
            Err(e) => Err(io::Error::other(e.to_string())),   // unplugged → reconnect logic kicks in
        }
    }
}

impl ByteWriter for UsbWriter {
    fn write(&mut self, data: &[u8]) -> io::Result<()> {
        self.handle.write_bulk(self.ep_out, data, Duration::from_millis(500)).map(|_| ()).map_err(|e| io::Error::other(e.to_string()))
    }
}

pub fn open_usb(vid: u16, pid: u16) -> Result<Transport, String> { /* open, claim interface, find endpoints */ }
```

Then a driver that finds the board by its USB id and opens it with the same `BoardDevice` and decoder:

```rust
pub struct UsbBoardDriver;

impl Driver for UsbBoardDriver {
    fn id(&self) -> &'static str { "board-usb" }
    fn discover(&self, _sys: &mut SystemPorts, out: &mut Vec<DeviceEntry>) {
        // for each USB device with your VID/PID: out.push(DeviceEntry { driver: self.id(), … })
    }
    fn open(&self, entry: &DeviceEntry, _opts: &OpenOptions, ctx: &egui::Context) -> Result<Box<dyn Device>, String> {
        let (reader, writer) = usb::open_usb(MY_VID, MY_PID)?;
        Ok(Box::new(BoardDevice::new(entry.clone(), "Analizator USB".into(), reader, writer,
                                     Box::new(TextFrameDecoder::default()), ctx)))
    }
}
```

Add it to `DRIVERS` in `src/device/mod.rs`. If the board uses USB CDC (it shows up as a serial
port), nothing has to change at all — the existing serial transport already covers it.

## Recipe: add a new kind of device

1. Create `src/device/mydevice.rs` (or a folder like `ppk2/` if it has a lot of protocol code).
2. Write a `Decoder` for its byte stream (or reuse one).
3. Write the device: a struct holding a `Link` (created with `Link::spawn(reader, decoder, ctx)`)
   and the writer, implementing `Device`. Only `name`, `entry`, `caps`, `link`, `start`, `stop`
   and `close` are required; everything else has defaults.
4. Write a `Driver`: `discover` adds `DeviceEntry`s (claim the serial ports you take with
   `sys.claim(path)`, so the generic board driver skips them), `open` opens the transport and
   returns the device. Opening runs on a background thread, so a handshake may take a moment.
5. Add `pub mod mydevice;` and the driver to `DRIVERS` in `src/device/mod.rs` — before
   `board::BoardDriver` if it uses serial ports, because the board driver takes every unclaimed port.
6. Optional: a settings panel (`panel_title` + `panel_ui`; use the widgets in `src/ui/widgets.rs`),
   log lines and preferences via `take_events`, settings to keep across a reconnect via
   `save_state` / `OpenOptions::restore`.
7. Test it without hardware over the in-memory `link::testing::Pipe` (see the tests in board.rs
   and ppk2/protocol.rs), or with a virtual serial port:

   ```sh
   socat pty,raw,echo=0,link=/tmp/ttyV0 pty,raw,echo=0,link=/tmp/ttyV1 &
   ANALIZATOR_EXTRA_PORTS=/tmp/ttyV0 cargo run    # a script on /tmp/ttyV1 plays the device
   ```

   `ANALIZATOR_PPK2_PORTS` does the same but makes the ports look like a PPK2.

## Debugging

* `ANALIZATOR_DEBUG=1` prints the plot drawing time per frame to stderr.
* The **Copy** button in the device log copies the whole log to the clipboard.
* Crashes write `crash_YYYYMMDD_HHMMSS.txt` (with a backtrace) into the logs folder.
