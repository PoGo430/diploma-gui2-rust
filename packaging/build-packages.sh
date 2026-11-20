#!/usr/bin/env bash
# Rebuild the ready-to-run packages in download/ (linked from the README).
# Needs: cargo-zigbuild (pip install ziglang cargo-zigbuild), mingw-w64, zip,
#        rustup targets x86_64-unknown-linux-gnu, aarch64-unknown-linux-gnu, x86_64-pc-windows-gnu.
# Usage: packaging/build-packages.sh            (run from anywhere)
set -euo pipefail
cd "$(dirname "$0")/.."

cargo zigbuild --release --target x86_64-unknown-linux-gnu.2.27
cargo zigbuild --release --target aarch64-unknown-linux-gnu.2.27
cargo build --release --target x86_64-pc-windows-gnu

rm -rf dist && mkdir -p dist download
for T in x86_64 aarch64; do
    D=dist/analizator-linux-$T
    mkdir -p "$D"
    cp "target/$T-unknown-linux-gnu/release/analizator" "$D/"
    cp packaging/install.sh packaging/README.txt assets/analizator.png "$D/"
    chmod +x "$D/analizator" "$D/install.sh"
    tar -C dist -czf "download/analizator-linux-$T.tar.gz" "analizator-linux-$T"
done
mkdir -p dist/Analizator
cp target/x86_64-pc-windows-gnu/release/analizator.exe dist/Analizator/Analizator.exe
sed 's/$/\r/' packaging/README-windows.txt > dist/Analizator/README.txt
rm -f download/analizator-windows-x86_64.zip
(cd dist && zip -q -9 -r ../download/analizator-windows-x86_64.zip Analizator)
rm -rf dist
ls -la download
