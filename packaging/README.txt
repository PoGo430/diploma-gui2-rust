ANALIZATOR — current & power analyzer
=====================================

QUICK START
  Option A (run directly): double-click "analizator".
      Some file managers (e.g. GNOME Files / Nautilus) don't start programs on
      double-click. Then right-click it -> "Run as a Program", or use option B.

  Option B (recommended): run the installer once:
      open a terminal in this folder and run   ./install.sh
      Analizator then appears in your applications menu and as a desktop icon.
      CSV files also get "Open with Analizator".
      To uninstall:   ./install.sh --remove

SERIAL PORT PERMISSION
  If connecting says "Permission denied", your user must be in the dialout
  group (install.sh offers to do this):
      sudo usermod -aG dialout $USER      then log out and back in.

NO HARDWARE?
  Pick "DEMO — simulated device" in the port list.

WHERE ARE MY FILES?
  Every recording is saved automatically as CSV:
    - next to the program in a "logs" folder (when run from a normal folder / USB stick)
    - or in ~/Analizator/logs (when installed)
  "Open logs folder" in the Device log panel opens it.

USING IT
  Mouse wheel ............ zoom (around the mouse pointer)
  Drag ................... pan
  Double-click a plot .... fit all data
  Shift + drag ........... make a selection (stats for that range)
  Drag selection edges ... resize; drag inside it to move
  Overview strip ......... click/drag to jump around a long capture
  Filter panel ........... Average / Median / Low-pass smoothing of the plots
                           (recordings are always saved unfiltered)
  Ctrl+R record/stop · Ctrl+O open · Ctrl+S export · Ctrl+F fit · Ctrl+E selection

REQUIREMENTS
  64-bit Linux from ~2018 or newer (glibc 2.27+), X11 or Wayland desktop, OpenGL.
  "analizator" is for normal PCs (x86_64); the arm64 package is for e.g. Raspberry Pi 4/5 with 64-bit OS.
