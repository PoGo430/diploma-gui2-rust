//! Analizator — current & power analyzer GUI.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;
mod csvio;
mod data;
mod filter;
mod fmt;
mod serial;

fn main() -> eframe::Result {
    let icon = egui::IconData { rgba: include_bytes!("../assets/icon_64.rgba").to_vec(), width: 64, height: 64 };
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("Analizator — Current & Power Analyzer")
            .with_app_id("analizator")
            .with_inner_size([1440.0, 880.0])
            .with_min_inner_size([960.0, 620.0])
            .with_icon(icon),
        ..Default::default()
    };
    let result = eframe::run_native("Analizator", options, Box::new(|cc| Ok(Box::new(app::AnalyzerApp::new(cc)))));
    if let Err(e) = &result {
        startup_error(&format!(
            "Analizator could not start:\n{e}\n\nThis usually means the graphics driver does not support OpenGL 2.0 \
             (common in virtual machines or over Remote Desktop). Updating the graphics driver usually fixes it."
        ));
    }
    result
}

/// The Windows build has no console window, so show startup errors in a message box.
#[cfg(windows)]
fn startup_error(text: &str) {
    #[link(name = "user32")]
    unsafe extern "system" {
        fn MessageBoxW(hwnd: *mut core::ffi::c_void, text: *const u16, caption: *const u16, kind: u32) -> i32;
    }
    let wide = |s: &str| s.encode_utf16().chain(std::iter::once(0)).collect::<Vec<u16>>();
    let (t, c) = (wide(text), wide("Analizator"));
    // SAFETY: both strings are valid, NUL-terminated UTF-16 buffers that outlive the call.
    unsafe {
        MessageBoxW(std::ptr::null_mut(), t.as_ptr(), c.as_ptr(), 0x10 /* MB_ICONERROR */);
    }
}

#[cfg(not(windows))]
fn startup_error(text: &str) {
    eprintln!("{text}");
}
