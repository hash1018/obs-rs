//! The D3D11 backend: DXGI desktop duplication straight into D3D11 textures,
//! a BGRA compositor, and a shared texture to reach wgpu without a readback.

mod shared;

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use eframe::egui;
use eframe::egui_wgpu::RenderState;
use media_pp::{
    buffer::MediaBuffer,
    elements::{
        AppSink, CaptureArea, CaptureMode, D3d11FrameRenderer, D3d11Renderer, D3d11VideoCompositor,
        D3d11VideoCompositorHandle, D3d11VideoCompositorInput, D3d11VideoLayerHandle,
        DxgiCaptureOptions, DxgiCaptureSource, SubmitError, TeeBuilder, VideoCompositorOptions,
        VideoLayer,
    },
    ffmpeg,
    pipeline::Pipeline,
    queue::OverflowPolicy,
};
use windows::Win32::Graphics::{
    Direct3D::{
        D3D_DRIVER_TYPE_HARDWARE, D3D_FEATURE_LEVEL, D3D_FEATURE_LEVEL_11_0, D3D_FEATURE_LEVEL_11_1,
    },
    Direct3D11::{
        D3D11_CREATE_DEVICE_BGRA_SUPPORT, D3D11_SDK_VERSION, D3D11CreateDevice, ID3D11Device,
        ID3D11DeviceContext, ID3D11Texture2D,
    },
    Dxgi::{CreateDXGIFactory1, IDXGIFactory1},
};

use crate::domain::{DisplayCaptureTarget, SourceKind, SourceSettings};
use crate::snapshots::SceneItemSnapshot;

use super::{BACKGROUND, BackendError, OpenSource, flat_bgra, input_name, unsupported_kind};

use shared::SharedTarget;

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

        // The composited frame reaches wgpu as a shared texture rather than a
        // readback: see `shared`. The copy into it is cheap enough that this
        // whole branch costs almost nothing, which is the point — the
        // `D3d11Download` it replaces measured as nearly this application's
        // entire GPU cost.
        let shared = SharedTarget::new(&device, render_state, width, height)?;
        let texture_id = shared.texture_id();

        // `on_frame` is not called from the rendering branch: that branch sits
        // behind a dropping queue and refreshes only at the Preview's rate,
        // while the rate reported has to be the compositor's — it is what says
        // whether an output could be made at the rate it is configured for.
        // So the frames are teed. A synchronous counting sink sees every
        // composited frame and makes every call; the rendering branch only
        // leaves a flag saying the texture was refreshed since the last one.
        let drawn_flag = Arc::new(AtomicBool::new(false));
        let count = {
            let drawn_flag = Arc::clone(&drawn_flag);
            AppSink::new("preview-rate", move |buffer| {
                if matches!(buffer, MediaBuffer::Video(_)) {
                    // A refreshed texture is reported one call late, which is
                    // one compositor tick after it actually changed.
                    on_frame(
                        drawn_flag
                            .swap(false, Ordering::Relaxed)
                            .then_some(texture_id),
                    );
                }
                Ok(())
            })
        };

        let renderer = D3d11Renderer::new(
            "preview-out",
            Box::new(PreviewRenderer {
                device: device.clone(),
                context: context.clone(),
                shared,
                drawn_flag,
                interval: Duration::from_secs_f32(1.0 / preview_fps as f32),
                last_drawn: Mutex::new(None),
            }),
        );

        let preview = Pipeline::new("preview", compositor, |source, context| {
            // The counting branch is synchronous — it is how the calls stay
            // at the compositor's own rate — so its sink must stay trivial.
            let count_branch = context.branch().to(Box::new(count))?;
            // The Preview must not set the compositor's pace, so the copy and
            // the repaint it asks for happen on this queue's worker, and the
            // queue drops whatever cannot keep up rather than making the
            // compositor wait.
            let draw_branch = context
                .branch()
                .queue_with_policy("preview-queue", 1, OverflowPolicy::DropNewest)
                .to(Box::new(renderer))?;
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

/// Puts each composited frame into the texture wgpu shares, at the Preview's
/// own rate.
///
/// A `D3d11FrameRenderer` normally presents to a window; this one presents to
/// egui, which draws the frame itself. `media-pp` still does the useful half:
/// it validates the frame, rejects one from another device, and hands over a
/// texture that is already exactly what has to be copied.
struct PreviewRenderer {
    device: ID3D11Device,
    context: Arc<Mutex<ID3D11DeviceContext>>,
    shared: SharedTarget,
    /// Set when the shared texture has new content the Preview has not been
    /// told about; the counting sink clears it as it reports.
    drawn_flag: Arc<AtomicBool>,
    /// `1 / preview_fps`. Copying is cheap, but every refreshed frame asks
    /// egui for a whole-UI repaint, and that is not.
    interval: Duration,
    last_drawn: Mutex<Option<Instant>>,
}

// SAFETY: the two COM handles are `windows-rs` interface wrappers, thread-safe
// to hold; every context call goes through `context`'s own mutex, and the
// device is only read from. The rest is plain data behind its own locks.
unsafe impl Send for PreviewRenderer {}
unsafe impl Sync for PreviewRenderer {}

impl D3d11FrameRenderer for PreviewRenderer {
    fn device(&self) -> ID3D11Device {
        self.device.clone()
    }

    unsafe fn submit_bgra_texture(
        &self,
        texture: ID3D11Texture2D,
        _array_index: u32,
        width: u32,
        height: u32,
    ) -> Result<(), SubmitError> {
        let mut last_drawn = self
            .last_drawn
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        // Dropped rather than copied: the copy would be cheap, the repaint it
        // leads to would not.
        if last_drawn.is_some_and(|last| last.elapsed() < self.interval) {
            return Ok(());
        }
        let copied = self
            .shared
            .copy_from(&self.context, &texture, width, height);
        if !copied {
            return Err(SubmitError::InvalidFrame);
        }
        *last_drawn = Some(Instant::now());
        self.drawn_flag.store(true, Ordering::Relaxed);
        Ok(())
    }

    unsafe fn submit_nv12_texture(
        &self,
        _texture: ID3D11Texture2D,
        _array_index: u32,
        _width: u32,
        _height: u32,
    ) -> Result<(), SubmitError> {
        // `D3d11VideoCompositor` emits BGRA and nothing else feeds this
        // renderer, so an NV12 frame arriving here is a graph that was not
        // built the way this backend builds it.
        Err(SubmitError::InvalidFrame)
    }

    fn resize(&self, _width: u32, _height: u32) -> Result<(), SubmitError> {
        // The target is the Scene Canvas, not the window: the Viewport scales
        // it while drawing, so a resized window changes nothing here.
        Ok(())
    }
}

/// The feature levels this backend asks for, best first.
///
/// 11_0 is the floor, and it is a real requirement rather than a
/// precaution: `D3d11VideoCompositor` compiles its shaders at
/// `vs_5_0`/`ps_5_0`, and shader model 5.0 is what feature level 11_0
/// means. Passing no list at all — the obvious thing to write — accepts
/// anything down to 9_1, so a machine below the line gets a device that
/// works right up until the compositor tries to create its shaders, and
/// fails there with an error naming none of this. Asking up front turns
/// that into one refusal at startup that says what is missing.
const FEATURE_LEVELS: [D3D_FEATURE_LEVEL; 2] = [D3D_FEATURE_LEVEL_11_1, D3D_FEATURE_LEVEL_11_0];

/// The device every D3D11 element here shares, and its immediate context.
///
/// `BGRA_SUPPORT` because everything this backend touches is BGRA: the
/// desktop duplication's own format, the compositor's working format, and
/// what the Preview download hands back. `media-pp` enables the context's
/// runtime multithread protection itself the moment the device reaches its
/// first element, so nothing more is done here.
///
/// Nothing about this is vendor-specific — `D3D_DRIVER_TYPE_HARDWARE` takes
/// whichever adapter the machine has, and desktop duplication is a Windows
/// API rather than one GPU maker's.
fn create_device() -> Result<(ID3D11Device, Arc<Mutex<ID3D11DeviceContext>>), BackendError> {
    let mut device = None;
    let mut context = None;
    // SAFETY: creates the documented device and context on the default
    // hardware adapter, reading `FEATURE_LEVELS` and writing only the two
    // out-parameters above.
    unsafe {
        D3D11CreateDevice(
            None,
            D3D_DRIVER_TYPE_HARDWARE,
            Default::default(),
            D3D11_CREATE_DEVICE_BGRA_SUPPORT,
            Some(&FEATURE_LEVELS),
            D3D11_SDK_VERSION,
            Some(&mut device),
            None,
            Some(&mut context),
        )
    }
    .map_err(|error| -> BackendError {
        format!(
            "no Direct3D 11 device at feature level 11_0, which the compositor's \
             shaders require: {error}"
        )
        .into()
    })?;
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
