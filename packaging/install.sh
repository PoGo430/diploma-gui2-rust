#!/bin/sh
# Analizator installer: adds Analizator to your applications menu (and desktop).
# Usage:  ./install.sh            install for the current user (no sudo needed)
#         ./install.sh --remove   uninstall
set -e
HERE="$(cd "$(dirname "$0")" && pwd)"
APP_DIR="$HOME/.local/share/analizator"
DESKTOP_FILE="$HOME/.local/share/applications/analizator.desktop"
DESKTOP_DIR="$(xdg-user-dir DESKTOP 2>/dev/null || echo "$HOME/Desktop")"

if [ "$1" = "--remove" ] || [ "$1" = "--uninstall" ]; then
    rm -rf "$APP_DIR" "$DESKTOP_FILE" "$DESKTOP_DIR/analizator.desktop"
    command -v update-desktop-database >/dev/null && update-desktop-database "$HOME/.local/share/applications" 2>/dev/null || true
    echo "Analizator removed. (Your captures in ~/Analizator were kept.)"
    exit 0
fi

mkdir -p "$APP_DIR" "$HOME/.local/share/applications"
cp "$HERE/analizator" "$APP_DIR/analizator"
cp "$HERE/analizator.png" "$APP_DIR/analizator.png"
chmod +x "$APP_DIR/analizator"

cat > "$DESKTOP_FILE" <<DESK
[Desktop Entry]
Type=Application
Name=Analizator
GenericName=Current & Power Analyzer
Comment=Capture and analyse current and power measurements
Exec="$APP_DIR/analizator" %f
Icon=$APP_DIR/analizator.png
Terminal=false
Categories=Development;Electronics;Science;
MimeType=text/csv;
StartupWMClass=analizator
Keywords=current;power;energy;serial;measurement;
DESK
chmod +x "$DESKTOP_FILE"
command -v update-desktop-database >/dev/null && update-desktop-database "$HOME/.local/share/applications" 2>/dev/null || true

# Desktop shortcut (double-click to start)
if [ -d "$DESKTOP_DIR" ]; then
    cp "$DESKTOP_FILE" "$DESKTOP_DIR/analizator.desktop"
    chmod +x "$DESKTOP_DIR/analizator.desktop"
    # GNOME asks "Allow launching?" for desktop shortcuts unless they are marked trusted
    command -v gio >/dev/null && gio set "$DESKTOP_DIR/analizator.desktop" metadata::trusted true 2>/dev/null || true
fi

echo "Analizator installed."
echo "  - Start it from your applications menu or the desktop icon."
echo "  - Captures are saved to ~/Analizator/logs"

# Serial ports on Linux need the 'dialout' group (or 'uucp' on Arch).
GROUP=dialout
getent group dialout >/dev/null 2>&1 || GROUP=uucp
if getent group "$GROUP" >/dev/null 2>&1 && ! id -nG "$USER" | tr ' ' '\n' | grep -qx "$GROUP"; then
    echo
    echo "Your user is not in the '$GROUP' group, so it cannot open serial ports yet."
    printf "Add it now? (asks for your password) [Y/n] "
    read -r ans || ans=n
    case "$ans" in
        n|N) echo "Skipped. Later run:  sudo usermod -aG $GROUP $USER   and log out/in." ;;
        *)   sudo usermod -aG "$GROUP" "$USER" && echo "Done. Log out and back in (or reboot) for it to take effect." ;;
    esac
fi
