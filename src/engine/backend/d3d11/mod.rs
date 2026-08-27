//! The D3D11 backend: DXGI desktop duplication straight into D3D11 textures,
//! a BGRA compositor, and one download to reach wgpu.

mod bgra;

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use eframe::egui;
use eframe::egui_wgpu::RenderState;
use media_pp::{
    buffer::MediaBuffer,
    elements::{
        AppSink, CaptureArea, CaptureMode, D3d11Download, D3d11VideoCompositor,
        D3d11VideoCompositorHandle, D3d11VideoCompositorInput, D3d11VideoLayerHandle,
        DxgiCaptureOptions, DxgiCaptureSource, TeeBuilder, VideoCompositorOptions, VideoLayer,
    },
    ffmpeg,
    pipeline::Pipeline,
    queue::OverflowPolicy,
};
use windows::Win32::Graphics::{
    Direct3D::D3D_DRIVER_TYPE_HARDWARE,
    Direct3D11::{
        D3D11_CREATE_DEVICE_BGRA_SUPPORT, D3D11_SDK_VERSION, D3D11CreateDevice, ID3D11Device,
        ID3D11DeviceContext,
    },
    Dxgi::{CreateDXGIFactory1, IDXGIFactory1},
};

use crate::domain::{DisplayCaptureTarget, SourceKind, SourceSettings};
use crate::snapshots::SceneItemSnapshot;

use super::{BACKGROUND, BackendError, OpenSource, flat_bgra, input_name, unsupported_kind};

use bgra::BgraTarget;

/// The compositor's layer control already offers exactly what a backend must.
pub(in crate::engine) type Layer = D3d11VideoLayerHandle;

pub(in crate::engine) struct Backend {
    device: ID3D11Device,
    compositor: D3d11VideoCompositorHandle,
    preview: Arc<Pipeline>,
}

impl Backend {
    pub(in crate::engine) fn start(
        render_state: &RenderState,
        size: [u32; 2],
        fps: u32,
        preview_fps: u32,
        on_frame: impl Fn(Option<egui::TextureId>) + Send + Sync + 'static,
    ) -> Result<Self, BackendError> {
        media_pp::init()?;
        let [width, height] = size;

        // One device for the whole stack: capture textures, compositor input,
        // and the download all have to live on it — and the compositor and
        // download must further share the immediate context's own `Arc`, not
        // merely the device behind it; see `D3d11VideoCompositor::new`.
        // Created on the default adapter, which is where the primary
        // display's duplication lands; a monitor on another adapter is
        // rejected by `open_with_device` rather than silently bridged
        // through system memory.
        let (device, context) = create_device()?;

        let target = BgraTarget::new(&render_state.device, width, height);
        let texture_id = render_state.renderer.write().register_native_texture(
            &render_state.device,
            target.output_view(),
            wgpu::FilterMode::Linear,
        );

        let (compositor, handle) = D3d11VideoCompositor::new(
            "preview-compositor",
            &device,
            context.clone(),
            VideoCompositorOptions {
                width,
                height,
                frame_rate: ffmpeg::Rational::new(fps as i32, 1),
                background: BACKGROUND,
            },
        )?;

        // The compositor works in D3D11 textures; this is the one place the
        // frame returns to system memory. Replacing this download with a
        // shared-texture import into wgpu is what removes the round trip.
        let download =
            D3d11Download::new("preview-download", &device, context.clone(), width, height)?;

        // Unlike the CUDA side, `on_frame` cannot be called from the drawing
        // sink here: `D3d11Download` maps a staging texture under the shared
        // context lock, which waits for the GPU, so its branch keeps up with
        // the Preview's rate rather than the compositor's and the dropping
        // queue in front of it eats the difference. Counting there would
        // report the download's pace as the compositor's — the exact
        // misattribution the status bar was once fixed for. So the frames are
        // teed: a synchronous counting sink sees every composited frame and
        // makes every `on_frame` call, while the drawing branch only leaves a
        // flag saying the texture was refreshed since the last call.
        let drawn_flag = Arc::new(AtomicBool::new(false));
        let count = {
            let drawn_flag = Arc::clone(&drawn_flag);
            AppSink::new("preview-rate", move |buffer| {
                if matches!(buffer, MediaBuffer::Video(_)) {
                    // Every composited frame, drawn or not: this is the
                    // compositor's rate, and it is the one that says whether
                    // an output could be made at the rate it is configured
                    // for. A drawn frame is reported one call late, which is
                    // one compositor tick after the texture already changed.
                    on_frame(
                        drawn_flag
                            .swap(false, Ordering::Relaxed)
                            .then_some(texture_id),
                    );
                }
                Ok(())
            })
        };

        // The drawing sink runs on the queue's own worker thread, so neither
        // the UI thread nor the engine's does the copy.
        let queue = render_state.queue.clone();
        let interval = Duration::from_secs_f32(1.0 / preview_fps as f32);
        let mut last_drawn: Option<Instant> = None;
        let sink = AppSink::new("preview-out", move |buffer| {
            let MediaBuffer::Video(video) = buffer else {
                return Ok(());
            };
            let due = last_drawn.is_none_or(|last| last.elapsed() >= interval);
            if due && target.draw(&queue, &video) {
                last_drawn = Some(Instant::now());
                drawn_flag.store(true, Ordering::Relaxed);
            }
            Ok(())
        });

        let preview = Pipeline::new("preview", compositor, |source, context| {
            // The counting branch is synchronous — it is how the calls stay
            // at the compositor's own rate — so its sink must stay trivial.
            let count_branch = context.branch().to(Box::new(count))?;
            // The Preview must not set the compositor's pace. The download's
            // GPU wait happens on this queue's worker, and the queue drops
            // whatever the download cannot keep up with rather than making
            // the compositor wait with it.
            let draw_branch = context
                .branch()
                .queue_with_policy("preview-queue", 1, OverflowPolicy::DropNewest)
                .pipe(download)
                .to(Box::new(sink))?;
            let tee_branch = TeeBuilder::new("preview-tee", context.clone())
                .branch(count_branch)
                .branch(draw_branch)
                .build()?;
            context.attach(source, 0, tee_branch)?;
            Ok(())
        })?;
        preview.run()?;

        Ok(Self {
            device,
            compositor: handle,
            preview,
        })
    }

    pub(in crate::engine) fn pause(&self) {
        self.preview.pause();
    }

    pub(in crate::engine) fn resume(&self) {
        self.preview.resume();
    }

    pub(in crate::engine) fn stop(&self) {
        self.preview.stop();
    }

    pub(in crate::engine) fn remove_source(&self, name: &str) {
        self.compositor.remove_source(name);
    }

    pub(in crate::engine) fn open_source(
        &self,
        item: &SceneItemSnapshot,
        layer: VideoLayer,
        fps: u32,
    ) -> Result<OpenSource, BackendError> {
        match item.kind {
            SourceKind::DisplayCapture => {
                open_display_capture(&self.device, &self.compositor, item, layer, fps)
            }
            SourceKind::Color => open_color_source(&self.device, &self.compositor, item, layer),
            _ => Err(unsupported_kind(item)),
        }
    }
}

/// The device every D3D11 element here shares, and its immediate context.
///
/// `BGRA_SUPPORT` because everything this backend touches is BGRA: the
/// desktop duplication's own format, the compositor's working format, and
/// what the Preview download hands back. `media-pp` enables the context's
/// runtime multithread protection itself the moment the device reaches its
/// first element, so nothing more is done here.
fn create_device() -> Result<(ID3D11Device, Arc<Mutex<ID3D11DeviceContext>>), BackendError> {
    let mut device = None;
    let mut context = None;
    // SAFETY: creates the documented device and context on the default
    // hardware adapter, writing only the two out-parameters above.
    unsafe {
        D3D11CreateDevice(
            None,
            D3D_DRIVER_TYPE_HARDWARE,
            Default::default(),
            D3D11_CREATE_DEVICE_BGRA_SUPPORT,
            None,
            D3D11_SDK_VERSION,
            Some(&mut device),
            None,
            Some(&mut context),
        )?;
    }
    let device = device.expect("D3D11CreateDevice succeeded with a device out-parameter");
    let context = context.expect("D3D11CreateDevice succeeded with a context out-parameter");
    Ok((device, Arc::new(Mutex::new(context))))
}

/// Feeds the compositor one frame of flat colour and leaves it there.
///
/// Pushed once rather than per frame: the compositor keeps the latest frame
/// each input gave it, and a colour that never changes never needs another.
/// Position, size and opacity are the layer's, so nothing here is redrawn
/// when the item moves.
fn open_color_source(
    device: &ID3D11Device,
    handle: &D3d11VideoCompositorHandle,
    item: &SceneItemSnapshot,
    layer: VideoLayer,
) -> Result<OpenSource, BackendError> {
    use media_pp::elements::{AppSource, D3d11Upload};

    let SourceSettings::Color(settings) = &item.settings else {
        return Err("scene item is not a color source".into());
    };
    let width = (settings.size[0].round() as u32).max(2) & !1;
    let height = (settings.size[1].round() as u32).max(2) & !1;

    let name = input_name(item);
    let (source, pusher) = AppSource::new(name.clone(), 1);
    // BGRA in, BGRA composited: unlike the CUDA side there is no colour-space
    // conversion between the upload and the compositor at all.
    let upload = D3d11Upload::new(format!("{name}-upload"), device, width, height);

    let D3d11VideoCompositorInput { sink, layer } = handle
        .add_source(name.clone(), layer)?
        .ok_or("the compositor is no longer running")?;
    let pipeline = Pipeline::new(name.clone(), source, move |source, context| {
        let branch = context.branch().pipe(upload).to(sink)?;
        context.attach(source, 0, branch)?;
        Ok(())
    })?;
    pipeline.run()?;
    pusher.push(flat_bgra(width, height, settings.rgba))?;

    Ok(OpenSource {
        pipeline,
        layer,
        name,
        refreshed_token: None,
        showing: true,
    })
}

/// Starts duplicating one display and wires it into the compositor.
fn open_display_capture(
    device: &ID3D11Device,
    handle: &D3d11VideoCompositorHandle,
    item: &SceneItemSnapshot,
    layer: VideoLayer,
    fps: u32,
) -> Result<OpenSource, BackendError> {
    let SourceSettings::DisplayCapture(settings) = &item.settings else {
        return Err("scene item is not a display capture".into());
    };
    let DisplayCaptureTarget::MonitorName(monitor) = &settings.target else {
        // A portal restore token belongs to a Wayland compositor; nothing on
        // Windows can resolve it, so a project moved across platforms gets an
        // error naming the actual problem rather than a capture of the wrong
        // display.
        return Err("a portal selection names no display Windows can resolve".into());
    };
    let output_index = resolve_output_index(monitor)?;

    let name = input_name(item);
    // GPU capture: the desktop lands in D3D11 textures on this backend's own
    // device and never reaches system memory. A monitor on another adapter is
    // rejected here rather than bridged through a CPU copy, which is the
    // point — a silent fallback would undo the whole arrangement.
    let (source, format) = DxgiCaptureSource::open_with_device(
        name.clone(),
        DxgiCaptureOptions {
            area: CaptureArea::Output { output_index },
            fps,
            capture_mode: CaptureMode::Gpu,
        },
        device,
    )?;
    eprintln!(
        "\"{}\": opened {} as output {} ({}x{})",
        item.name, monitor, output_index, format.width, format.height,
    );

    // Capture gives BGRA D3D11 textures and the compositor takes exactly
    // those, so unlike the CUDA side nothing sits between them.
    let D3d11VideoCompositorInput { sink, layer } = handle
        .add_source(name.clone(), layer)?
        .ok_or("the compositor is no longer running")?;
    let pipeline = Pipeline::new(name.clone(), source, move |source, context| {
        let branch = context.branch().to(sink)?;
        context.attach(source, 0, branch)?;
        Ok(())
    })?;
    pipeline.run()?;

    Ok(OpenSource {
        pipeline,
        layer,
        name,
        refreshed_token: None,
        showing: true,
    })
}

/// Resolves a stable display name such as `\\.\DISPLAY1` to the flat output
/// index [`CaptureArea::Output`] takes — adapter 0's outputs, then adapter
/// 1's, matching that variant's own documented order.
///
/// Resolved at open time against whatever layout is live, not persisted: the
/// name is the stable half, the index is whatever it maps to today.
fn resolve_output_index(monitor: &str) -> Result<u32, BackendError> {
    // SAFETY: enumeration creates and reads only its own COM objects, and
    // `GetDesc` writes one fully-sized descriptor into a live local.
    unsafe {
        let factory: IDXGIFactory1 = CreateDXGIFactory1()?;
        let mut flat_index = 0u32;
        for adapter_index in 0.. {
            let Ok(adapter) = factory.EnumAdapters1(adapter_index) else {
                break;
            };
            for output_index in 0.. {
                let Ok(output) = adapter.EnumOutputs(output_index) else {
                    break;
                };
                let desc = output.GetDesc()?;
                let name_end = desc
                    .DeviceName
                    .iter()
                    .position(|unit| *unit == 0)
                    .unwrap_or(desc.DeviceName.len());
                if String::from_utf16_lossy(&desc.DeviceName[..name_end]) == monitor {
                    return Ok(flat_index);
                }
                flat_index += 1;
            }
        }
    }
    Err(format!("display \"{monitor}\" was not found in the current layout").into())
}
