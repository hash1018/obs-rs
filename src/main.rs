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
    // Held for the whole process: dropping it stops `media-pp`'s file logger.
    let _log = start_media_pp_log();

    let mut options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("obs-rs")
            .with_inner_size([960.0, 600.0])
            .with_min_inner_size([480.0, 320.0]),
        renderer: eframe::Renderer::Wgpu,
        ..Default::default()
    };
    pin_windows_backend(&mut options);

    eframe::run_native(
        "obs-rs",
        options,
        Box::new(|cc| Ok(Box::new(ObsApp::new(cc)))),
    )
}

/// Restricts wgpu to Direct3D 12 on Windows.
///
/// Not a preference: the D3D11 compositor hands the Preview its frames as
/// shared textures, and opening one is `ID3D12Device::OpenSharedHandle`.
/// Left to itself wgpu picks Vulkan here, where the same import means
/// `VK_KHR_external_memory_win32` and building the image by hand — a second
/// interop path to write and keep working for no benefit. Every Windows GPU
/// that runs this application's D3D11 compositor has a D3D12 driver too, so
/// nothing is excluded by asking for it.
#[cfg(target_os = "windows")]
fn pin_windows_backend(options: &mut eframe::NativeOptions) {
    if let eframe::egui_wgpu::WgpuSetup::CreateNew(setup) = &mut options.wgpu_options.wgpu_setup {
        setup.instance_descriptor.backends = eframe::wgpu::Backends::DX12;
    }
}

#[cfg(not(target_os = "windows"))]
fn pin_windows_backend(_options: &mut eframe::NativeOptions) {}

/// Turns on `media-pp`'s own file log, beside this user's project database.
///
/// The library keeps a private logger rather than emitting through `log` or
/// `tracing`, so nothing here is installed process-wide and this is the only
/// way to see what the pipelines are doing. Failing to open it is not worth
/// refusing to start over — the application runs perfectly well without a
/// log — so this reports and carries on.
///
/// `OBS_RS_MEDIA_PP_LOG` raises or lowers the threshold; `info` is what a
/// normal run wants, and chasing a frame through the graph wants `trace`.
fn start_media_pp_log() -> Option<media_pp::log::LogGuard> {
    use media_pp::log::Level;

    let level = match std::env::var("OBS_RS_MEDIA_PP_LOG").as_deref() {
        Ok("error") => Level::Error,
        Ok("warn") => Level::Warn,
        Ok("debug") => Level::Debug,
        Ok("trace") => Level::Trace,
        _ => Level::Info,
    };
    let directory = paths::data_dir().join("logs");
    match media_pp::log::init("media-pp", &directory.to_string_lossy(), level, 7) {
        Ok(guard) => Some(guard),
        Err(error) => {
            eprintln!("media-pp logging is off: {error}");
            None
        }
    }
}
