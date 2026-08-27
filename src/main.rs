//! obs-rs — a live capture, compositing, and recording app built on `media-pp`.
//!
//! This is the shell it starts from: one egui window, drawn through wgpu.
//! wgpu is deliberately the renderer from the very first commit, because the
//! preview will show frames that `media-pp` produced on its own GPU device
//! and hands over as an imported texture, and that import is a wgpu-level
//! operation. Starting on the default `glow` backend would mean rewriting
//! this later for nothing.

// Windows: no console window behind the GUI in release builds. Debug keeps
// it, because that is where panics and logs show up while developing.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;
mod capture;
mod domain;
mod engine;
mod i18n;
mod paths;
mod persistence;
mod project;
mod resources;
mod settings;
mod snapshots;
mod ui;

use eframe::egui;

use app::ObsApp;

fn main() -> eframe::Result {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("obs-rs")
            .with_inner_size([960.0, 600.0])
            .with_min_inner_size([480.0, 320.0]),
        renderer: eframe::Renderer::Wgpu,
        ..Default::default()
    };

    eframe::run_native(
        "obs-rs",
        options,
        Box::new(|cc| Ok(Box::new(ObsApp::new(cc)))),
    )
}
