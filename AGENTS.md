# Instructions for AI assistants (and new developers)

Read this whole file before changing anything. It is the briefing for any AI assistant
(Claude, ChatGPT, Copilot, Cursor, Gemini, …) asked to modify this project. If you are
pasting the project into a chat that cannot read files, paste this file first, then
[DEVELOPING.md](DEVELOPING.md), then the source files relevant to the change.

## 1. What this project is

**Analizator** is a desktop GUI for a current-measurement board the owner built for their
diploma thesis (the hardware and firmware are the thesis; this GUI is a tool, not part of
the thesis). It also supports the **Nordic Power Profiler Kit II (PPK2)**.

It records current samples (µA) from the device, plots them live, and analyses them:
statistics, filters, markers, battery-life estimate, sleep/active breakdown, CSV files.

* Language: **Rust (edition 2024)**, GUI with **egui/eframe 0.36** + **egui_plot 0.37**.
* Ships as a **single executable** (double-click to run) for Linux x86_64, Linux aarch64
  (Raspberry Pi) and Windows x86_64. Prebuilt packages are committed in `download/`.
* Repository: `PoGo430/diploma-gui2-rust`, branch `main`.

## 2. The owner's decisions — respect these

These were decided explicitly. Do not add them back "to be helpful".

**Never:**
* **Never write to disk while recording** (no streaming to disk, no periodic flush of the
  running capture). Data stays in memory; the capture is saved as CSV *after* it stops
  (auto-save), on export, or when the device disconnects.

**Rejected — do not add unless the owner asks:** timed/auto-stop recording, triggers,
save plot as image, light theme, measurement report generation, keyboard navigation of
the plot, recent-files list, Y-axis lock, automatic event detection, zero/offset
calibration in the GUI (done in the board's firmware), background-thread filter
computation.

**Undecided — ask first:** log Y axis, histogram, overlaying two captures, FFT.

**Wanted style:** dark theme only, compact, fast with tens of millions of samples, no
feature bloat. Fewer, solid features beat many half-done ones. Ask before adding a big
feature; small fixes and polish are fine.

## 3. Hard technical constraints

* Must stay a **single self-contained executable**: no runtime files, no installer
  required, no Python, no web view. Add crate dependencies sparingly (each one grows the
  binary and the build); prefer pure-Rust crates that cross-compile to Windows with MinGW
  and to `aarch64` with zig.
* Linux builds target **glibc 2.27** (`cargo zigbuild --target …-gnu.2.27`). Don't add
  dependencies that need newer system libraries or extra `.so` files at runtime.
* `serialport` is used **without default features** (no libudev). Linux port discovery
  reads `/sys` directly (`src/device/ports.rs`).
* The GUI thread must never block: device I/O runs on the `Link` reader thread, opening a
  device runs on a background thread, file saving runs on a background thread.
* Memory: up to `MAX_SAMPLES = 50_000_000` samples. Samples are `f64` µA (8 bytes each);
  evenly spaced captures don't store x positions (`Positions::Even`). Keep per-sample memory small.
* Plotting uses min/max decimation over 256-sample blocks (`data.rs`) so it stays fast;
  don't plot raw points over large ranges.
* The app keeps its own view range and handles zoom itself (egui_plot's zoom/scroll/reset are
  disabled, only its x-drag is used for panning); see `plots.rs`.

## 4. Code map

```
src/
  main.rs              window setup, icon, crash handler (crash_*.txt into logs folder)
  app/                 the GUI — talks to devices ONLY through the `Device` trait
    mod.rs             AnalyzerApp state, Settings (persisted), Mode enum, per-frame loop
    connection.rs      device list, connect (background thread), auto-reconnect, record/stop/dump
    files.rs           CSV open/save/export (background), drag & drop, logs folder
    topbar.rs          toolbar, status bar, keyboard shortcuts
    sidebar.rs         Measurement, Filter, Statistics, Battery & Sleep, Markers, Device log
    plots.rs           current/power plots, overview strip, right-click menu
    markers.rs         markers (label, drag, select between)
    dialogs.rs         confirmations, error box, About/F1 help
  ui/theme.rs          colours (dark theme) — use these constants, don't hard-code colours
  ui/widgets.rs        button, button_why (disabled reason tooltip), section, card, tile, …
  device/              everything device-specific
    mod.rs             Device + Driver traits, DRIVERS registry, discovery, find_again
    link.rs            ByteReader/ByteWriter (transport), Decoder, Link (reader thread)
    ports.rs           serial ports: listing (with USB VID/PID/serial), opening
    board.rs           the thesis board: protocol constants, TextFrameDecoder, BoardDevice, BoardDriver
    demo.rs            simulated board ("DEMO — simulated device")
    ppk2/protocol.rs   PPK2 bytes ↔ samples (commands, calibration, decoder) — no GUI code
    ppk2/mod.rs        PPK2 device, its sidebar panel, driver
  data.rs              DataStore/Series: samples + 256-sample min/max/sum blocks
  filter.rs            moving average, median, first-order low-pass
  csvio.rs             capture file read/write
  fmt.rs               number/time/axis formatting (SI prefixes)
```

[DEVELOPING.md](DEVELOPING.md) explains the device layer in detail and has copy-paste
recipes for: changing the board protocol, moving the board to native USB, adding a new
device.

### Device layer in one paragraph

Transport (`ByteReader`/`ByteWriter`: serial port, demo simulator, in-memory `Pipe` in
tests) → `Decoder` (bytes → samples in µA, text lines, received/damaged counters) →
`Device` (commands, capabilities, optional sidebar panel). A `Driver` discovers and opens
devices of one kind; `device::DRIVERS` lists them in order (PPK2 → board → demo). Drivers
`claim` the serial ports they take; the board driver takes every unclaimed port, so new
serial drivers go **before** it. The GUI decides what to enable from `Device::caps()`.

## 5. Protocols and formats (single source of truth is the code)

**Board** (`src/device/board.rs`, `protocol` module): text over serial, default 921600 baud
(user-selectable).
* Board → PC: `$DB<v1>,<v2>,…,<vN>$DE\n` — current samples in µA. Any other line goes to the
  device log. Malformed frames are counted as "corrupted".
* PC → board: `START`, `STOP`, `DUMP`, `ERASE` + `\n`.
* One sample = 1 ms by default (user-settable sample period); voltage is not measured, the
  user enters the DUT voltage (default 3.3 V).
* Dump has no end marker: it ends after 1.5 s without data (`DUMP_IDLE_TIMEOUT`, connection.rs).

**PPK2** (`src/device/ppk2/protocol.rs`): USB VID 0x1915 / PID 0xC00A, two CDC serial
ports. Commands, calibration metadata and the 4-byte sample format follow Nordic's
pc-nrfconnect-ppk and IRNAS ppk2-api-python; constants and formulas are documented in
the file. 100 kHz raw, optionally averaged to 10 / 1 kHz. Sample counter → lost-sample
detection. Bits 24–31 (logic inputs D0–D7) are currently ignored.

**Capture CSV** (`src/csvio.rs`, documented in README "Capture file format"): `# key=value`
header (voltage_V, sample_period_ms, start_index, saved, marker=idx,label), then
`Current_uA`, one value per line. Keep **backward compatibility**: old 4-column files and
Nordic Power Profiler exports must still open. If you change the format, add a loader
test.

## 6. Conventions

* Format with `cargo fmt` (rustfmt.toml: 120 columns). CI runs
  `cargo clippy --all-targets -- -D warnings` — **zero warnings**.
* Units: currents are **µA** (`f64`), time in seconds in the
  GUI, sample period in ms in settings/files. Format numbers with `fmt.rs` (SI prefixes).
* UI text is English, short, sentence case. Every disabled button gets a reason via
  `button_why`. Some glyphs are missing from egui's default font (e.g. ●, →, ✕) — use
  the ones already in the code (`widgets::dot`, ➡, 🗙) and check visually.
* Persisted settings live in `Settings` (app/mod.rs, serde). Adding a field: give it a
  default; **renaming** one resets users' value unless you add `#[serde(alias = "old")]`.
* Device-specific code stays in `src/device/`. The GUI must not match on device types.
* Errors shown to the user are plain sentences with a hint what to do (see `open_serial`).
* Tests live next to the code (`#[cfg(test)] mod tests`). Devices are tested over
  `link::testing::Pipe` without hardware.

## 7. Building, testing, verifying

```sh
cargo build --release && ./target/release/analizator   # optional: a .csv path as argument
cargo test
cargo clippy --all-targets -- -D warnings
cargo fmt
```

Without hardware: choose **DEMO** in the device list. Fake serial devices:

```sh
socat pty,raw,echo=0,link=/tmp/ttyV0 pty,raw,echo=0,link=/tmp/ttyV1 &
ANALIZATOR_EXTRA_PORTS=/tmp/ttyV0 cargo run     # a script writing frames to /tmp/ttyV1 plays the board
ANALIZATOR_PPK2_PORTS=/tmp/ttyV0 cargo run      # same port, treated as a PPK2
```

`ANALIZATOR_DEBUG=1` prints plot timings. GUI changes should be checked by actually
running the app (a screenshot), not only by compiling — e.g. under `Xvfb` with `xdotool`
when no display is available.

Testing status: the PPK2 support has only been tested against a simulator (no real PPK2
yet); the Windows build has not been run on a real Windows PC yet.

## 8. Releasing a change (checklist)

1. `cargo fmt`, `cargo clippy --all-targets -- -D warnings`, `cargo test` all clean.
2. Run the app and check the change works.
3. Bump `version` in `Cargo.toml` for user-visible changes (shown in the About box).
4. Update **README.md** (features, controls, file format) and **DEVELOPING.md** /
   this file if structure or conventions changed.
5. Rebuild the packages in `download/`: `packaging/build-packages.sh` (needs cargo-zigbuild,
   mingw-w64, zip and the three rustup targets — see the script header). The README links
   to these files, so they must match the code.
6. Commit and push to `main`. CI (`.github/workflows/rust.yml`) builds, tests and lints.
   Pushing a `v*` tag runs `.github/workflows/release.yml`, which publishes a GitHub
   release (notes from `.github/release-notes.md`).

## 9. Ideas discussed but not done (ask the owner before starting)

* PPK2 logic channels D0–D7 shown as digital traces under the plot.
* Per-interval table between markers (duration, avg current, charge/energy).
* Compressed `.csv.gz` save for very large captures (still saved only after stopping).
* Per-sample measured voltage, if a future board revision measures it.
* Firmware-side (owner's code, "maybe later"): binary frames, end-of-dump marker, frame
  sequence numbers.
