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
    eframe::run_native("Analizator", options, Box::new(|cc| Ok(Box::new(app::AnalyzerApp::new(cc)))))
}
