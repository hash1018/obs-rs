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
mod hotkey;
mod i18n;
mod instance;
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
    // Before anything opens a file of its own. Two instances would write one
    // log and one project database between them, so the second is turned away
    // ahead of both — see `instance`.
    let _instance = match instance::claim() {
        instance::Claim::Ours(instance) => instance,
        instance::Claim::Taken { pid } => {
            // The user asked for obs-rs and there is one. Showing it is what
            // they meant; saying nothing at all would read as a failed launch.
            if !pid.is_some_and(instance::raise) {
                eprintln!("obs-rs is already running");
            }
            return Ok(());
        }
    };

    // Held for the whole process: dropping it stops `media-pp`'s file logger.
    let _log = start_media_pp_log();

    // Read here rather than only in `ObsApp`, because where the window opens
    // has to be decided before there is one. The same values are handed on,
    // so the file is read once.
    let store = settings::SettingsStore::for_current_user();
    let settings = store.load().unwrap_or_else(|error| {
        eprintln!("could not load app settings: {error}");
        settings::AppSettings::default()
    });

    let mut options = eframe::NativeOptions {
        viewport: place_window(
            egui::ViewportBuilder::default()
                .with_title("obs-rs")
                // What a Wayland compositor matches this window to its
                // desktop entry by, and the only way it can find an icon
                // for it: the protocol has no call for a client to set
                // one, so `with_icon` above is X11's answer and this is
                // Wayland's. Set explicitly because nothing sets it
                // otherwise — `run_native`'s name goes to the storage
                // directory, and `egui-winit` names the surface only when
                // this field is `Some`, so leaving it out is a window with
                // no `app_id` at all and a task bar showing the fallback
                // icon beside the right name.
                .with_app_id("obs-rs")
                .with_icon(window_icon())
                .with_min_inner_size([480.0, 320.0]),
            settings.workspace.window,
        ),
        renderer: eframe::Renderer::Wgpu,
        ..Default::default()
    };
    pin_windows_backend(&mut options);
    request_vulkan_interop(&mut options);

    eframe::run_native(
        "obs-rs",
        options,
        Box::new(move |cc| Ok(Box::new(ObsApp::new(cc, store, settings)))),
    )
}

/// The icon the window and the task bar show.
///
/// Separate from the one `build.rs` stamps onto the executable, and not
/// redundant with it: an application that sets a window icon overrides what
/// the `.exe` carries, so leaving this out on Windows means the resource is
/// there and a default is shown anyway. On Linux there is no executable
/// resource to fall back to at all.
///
/// A failure here is not worth refusing to start over — the window opens with
/// whatever the toolkit defaults to, which is what it did before there was an
/// icon.
fn window_icon() -> egui::IconData {
    const PNG: &[u8] = include_bytes!("../assets/obs-rs.png");

    match eframe::icon_data::from_png_bytes(PNG) {
        Ok(icon) => icon,
        Err(error) => {
            eprintln!("could not read the window icon: {error}");
            egui::IconData::default()
        }
    }
}

/// The window's own default size, used until the application has closed once.
const DEFAULT_WINDOW_SIZE: [f32; 2] = [960.0, 600.0];

/// Puts the window back where it was left, if that is still somewhere the
/// user can reach.
///
/// A saved position is not trusted on its own: a window left on a second
/// display opens off-screen when that display is gone, and there is nothing
/// on screen to drag back. So the position is only asked for when it still
/// lands on a monitor this session has — and the *size* is restored either
/// way, since a size is never unreachable.
fn place_window(
    viewport: egui::ViewportBuilder,
    saved: Option<settings::WindowGeometry>,
) -> egui::ViewportBuilder {
    let Some(saved) = saved.filter(|saved| saved.width > 0.0 && saved.height > 0.0) else {
        return viewport.with_inner_size(DEFAULT_WINDOW_SIZE);
    };
    let viewport = viewport
        .with_inner_size([saved.width, saved.height])
        .with_maximized(saved.maximized);
    // No position at all on a platform that will not report one, which is
    // every Wayland session — see `WindowGeometry`.
    let (Some(x), Some(y)) = (saved.x, saved.y) else {
        return viewport;
    };
    if on_a_display(x, y) {
        viewport.with_position([x, y])
    } else {
        viewport
    }
}

/// Whether the window's top-left corner is inside one of this session's
/// displays.
///
/// The corner rather than the whole rect: a window hanging off the right edge
/// is one the user can still see and move, while one whose title bar is past
/// every monitor is not. An empty list — Wayland, where enumeration is the
/// portal's job — answers `true`, since refusing to restore on the ground
/// that nothing could be enumerated would be worse than trusting the file.
fn on_a_display(x: f32, y: f32) -> bool {
    let monitors = capture::displays();
    monitors.is_empty()
        || monitors.iter().any(|area| {
            x >= area.x as f32
                && y >= area.y as f32
                && x < area.x as f32 + area.width as f32
                && y < area.y as f32 + area.height as f32
        })
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

/// Asks wgpu for the one Vulkan extension the CUDA interop needs, on Linux.
///
/// The composited frame reaches wgpu as memory both APIs hold, and the
/// Vulkan half of that is `VK_KHR_external_memory_fd`: the buffer's memory
/// is exported as a file descriptor and `cuImportExternalMemory` takes
/// exactly that. wgpu enables the extension only when the device is asked
/// for `VULKAN_EXTERNAL_MEMORY_FD`, and eframe is what creates the device,
/// so the asking has to happen here.
///
/// An adapter that cannot offer it gets a device without it, and the backend
/// then refuses to start with an error naming what is missing — rather than
/// quietly going back to copying every frame through system memory.
#[cfg(target_os = "linux")]
fn request_vulkan_interop(options: &mut eframe::NativeOptions) {
    use eframe::wgpu;

    let eframe::egui_wgpu::WgpuSetup::CreateNew(setup) = &mut options.wgpu_options.wgpu_setup
    else {
        return;
    };
    // Vulkan is what wgpu picks here anyway; naming it makes that the only
    // outcome, since the import is written against no other backend.
    setup.instance_descriptor.backends = wgpu::Backends::VULKAN;
    let inner = std::sync::Arc::clone(&setup.device_descriptor);
    setup.device_descriptor = std::sync::Arc::new(move |adapter| {
        let mut descriptor = inner(adapter);
        // Intersected with what the adapter has: asking for a feature it
        // does not offer fails device creation outright, which would take
        // the whole application down over the Preview's copy path.
        descriptor.required_features |=
            wgpu::Features::VULKAN_EXTERNAL_MEMORY_FD & adapter.features();
        descriptor
    });
}

#[cfg(not(target_os = "linux"))]
fn request_vulkan_interop(_options: &mut eframe::NativeOptions) {}

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
