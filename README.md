# Analizator — current & power analyzer GUI

Desktop app for capturing and analysing current measurements from the Analizator board over a serial port. Written in Rust ([egui](https://github.com/emilk/egui) + [egui_plot](https://github.com/emilk/egui_plot)); compiles to a single Linux executable with no runtime dependencies besides the system C library.

![screenshot](docs/screenshot.png)

## Download

| File | For |
|---|---|
| **[analizator-linux-x86_64.tar.gz](https://github.com/PoGo430/diploma-gui2-rust/raw/main/download/analizator-linux-x86_64.tar.gz)** | Normal PCs and laptops (64-bit Intel/AMD) |
| [analizator-linux-aarch64.tar.gz](https://github.com/PoGo430/diploma-gui2-rust/raw/main/download/analizator-linux-aarch64.tar.gz) | Raspberry Pi 4/5 with 64-bit OS |

Runs on any 64-bit Linux with glibc 2.27+ (Ubuntu 18.04, Debian 10, Fedora 28 or newer); nothing else to install.

```sh
tar xzf analizator-linux-x86_64.tar.gz
cd analizator-linux-x86_64
./install.sh        # adds a menu entry and desktop icon (optional)
./analizator        # or just double-click it
```

If connecting fails with "Permission denied", run `sudo usermod -aG dialout $USER` and log out and back in (`install.sh` offers to do this). No hardware? Pick **DEMO — simulated device** in the port list.

## Features

- Live capture with current and power plots plus an overview strip for navigating long recordings
- Min/max ("peak") decimation, so short current spikes stay visible at any zoom level; stays smooth with tens of millions of samples
- Statistics for the whole capture, a selection, or the visible window: duration, avg/RMS/peak/min current, avg/peak power, charge, energy
- Hover readout of time, current and power at the cursor
- Flash dump / erase, auto-save of every capture to CSV, CSV import and export
- Built-in **DEMO** device for trying the app without hardware
- Settings are remembered between runs

## Serial protocol

| Direction | Format |
|---|---|
| Board → PC | `$DB<v1>,<v2>,...,<vN>$DE\n`: current samples in µA. Any other line is shown in the device log. |
| PC → board | `START`, `STOP`, `DUMP`, `ERASE` (newline terminated) |

CSV format: `Index,Current_uA,Voltage_V,Power_uW`

## Controls

| Action | Input |
|---|---|
| Zoom | Mouse wheel (around the pointer) |
| Pan | Drag |
| Fit all data | Double-click, or Ctrl+F |
| Make selection | Shift + drag (then drag edges to resize, middle to move) |
| Jump around | Click/drag in the overview strip |
| Record / stop | Ctrl+R |
| Open / export CSV | Ctrl+O / Ctrl+S |
| Toggle selection stats | Ctrl+E |

## Building

Install Rust from <https://rustup.rs>, then:

```sh
cargo build --release
./target/release/analizator            # optionally: ./target/release/analizator capture.csv
cargo test                             # unit tests
```

### Portable build for older Linux distributions

A normal build links against the build machine's glibc. To produce a binary that runs on any distribution with glibc 2.27+ (Ubuntu 18.04 and newer), use [cargo-zigbuild](https://github.com/rust-cross/cargo-zigbuild):

```sh
pip install ziglang cargo-zigbuild
cargo zigbuild --release --target x86_64-unknown-linux-gnu.2.27
# Raspberry Pi 4/5 (64-bit OS):
rustup target add aarch64-unknown-linux-gnu
cargo zigbuild --release --target aarch64-unknown-linux-gnu.2.27
```

### Packaging

Put the binary next to `packaging/install.sh`, `packaging/README.txt` and `assets/analizator.png`, then archive it with `tar czf` (this keeps the executable bit). Users run `./install.sh` to add a menu entry and desktop icon; `./install.sh --remove` uninstalls.

## Serial port permission (Linux)

Opening a serial port requires membership in the `dialout` group (`uucp` on Arch):

```sh
sudo usermod -aG dialout $USER   # then log out and back in
```

## Source layout

| File | Purpose |
|---|---|
| `src/main.rs` | Window setup |
| `src/app.rs` | User interface, plotting, capture logic |
| `src/data.rs` | Sample storage with per-256-sample min/max/sum blocks (fast drawing and statistics) |
| `src/serial.rs` | Port listing, reader thread, frame parser, DEMO board |
| `src/csvio.rs` | CSV load/save |
| `src/fmt.rs` | SI-prefix number formatting |

## Releases

Ready-to-run Linux packages are on the [Releases](../../releases) page. To publish a new version, push a tag:

```sh
git tag v2.0.1
git push origin v2.0.1
```

The [release workflow](.github/workflows/release.yml) then tests, builds the x86_64 and arm64 packages, and publishes the release automatically.
