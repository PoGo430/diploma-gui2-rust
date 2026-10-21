ANALIZATOR — current & power analyzer (Windows)
===============================================

QUICK START
  1. Extract the whole zip (right-click -> Extract All...).
  2. Double-click Analizator.exe.

  The first time, Windows may show "Windows protected your PC" because the
  program is not signed. Click "More info" -> "Run anyway".

  No hardware? Pick "DEMO — simulated device" in the port list.

CONNECTING THE BOARD
  The board shows up as a COM port (e.g. "COM3 — ..."). Windows 10/11 has the
  driver for USB CDC devices built in. Boards with a CH340 or CP210x USB chip
  need that chip's driver from the manufacturer.
  "Access is denied" when connecting means another program (serial monitor,
  IDE, a second Analizator window) has the port open; close it first.

WHERE ARE MY FILES?
  Every recording is saved automatically as CSV in a "logs" folder next to
  Analizator.exe (or in %USERPROFILE%\Analizator\logs if that folder is not
  writable, e.g. under Program Files). "Open logs folder" in the Device log
  panel opens it.

USING IT
  Mouse wheel ............ zoom (around the mouse pointer)
  Drag ................... pan
  Double-click a plot .... fit all data
  Shift + drag ........... make a selection (stats for that range)
  Drag selection edges ... resize; drag inside it to move
  M ...................... add a marker (right-click a plot for more)
  Overview strip ......... click/drag to jump around a long capture
  Filter panel ........... Average / Median / Low-pass smoothing of the plots
                           (recordings are always saved unfiltered)
  Ctrl+R record/stop · Ctrl+O open · Ctrl+S export · Ctrl+F fit · Ctrl+E selection

REQUIREMENTS
  64-bit Windows 10 or 11 with a graphics driver that supports OpenGL 2.0
  (any normal PC/laptop; may fail inside a virtual machine or over Remote Desktop).
