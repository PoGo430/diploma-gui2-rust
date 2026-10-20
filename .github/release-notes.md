Analizator — current & power analyzer (Rust version).

## Download

| File | For |
|---|---|
| **analizator-linux-x86_64.tar.gz** | Normal PCs / laptops (64-bit Intel/AMD) |
| analizator-linux-aarch64.tar.gz | Raspberry Pi 4/5 with 64-bit OS |

Runs on any 64-bit Linux with glibc 2.27+ (Ubuntu 18.04 / Debian 10 / Fedora 28 or newer). Nothing else needs to be installed.

## Install

```sh
tar xzf analizator-linux-x86_64.tar.gz
cd analizator-linux-x86_64
./install.sh        # adds a menu entry and desktop icon
```

Or just double-click `analizator` (in GNOME Files: right-click → *Run as a Program*).

If connecting fails with "Permission denied", run `sudo usermod -aG dialout $USER` and log out/in (`install.sh` offers to do this).

No hardware? Choose **DEMO — simulated device** in the port list.
