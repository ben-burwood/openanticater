//! Anticater VK-01 — egui + wgpu 3D viewer.
//!
//! Renders the `hardware/vk01.scad` knob in real time: choose the LED seam
//! colour, pulse it, spin the knob (with momentum) and press it in.

// Hide the console window in release builds on Windows.
#![cfg_attr(
    all(target_os = "windows", not(debug_assertions)),
    windows_subsystem = "windows"
)]

mod app;
mod renderer;

use eframe::egui;

fn main() -> eframe::Result<()> {
    let native_options = eframe::NativeOptions {
        renderer: eframe::Renderer::Wgpu,
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1000.0, 680.0])
            .with_min_inner_size([680.0, 460.0])
            .with_title("Anticater VK-01 Viewer"),
        ..Default::default()
    };

    eframe::run_native(
        "Anticater VK-01 Viewer",
        native_options,
        Box::new(|cc| Ok(Box::new(app::App::new(cc)))),
    )
}
