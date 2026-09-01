//! The D3D11 backend: DXGI desktop duplication straight into D3D11 textures,
//! a BGRA compositor, and a shared texture to reach wgpu without a readback.

use crate::engine::TARGET_FPS;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use eframe::egui;
use eframe::egui_wgpu::RenderState;
use media_pp::{
    buffer::MediaBuffer,
    elements::{
        AppSink, ChangeGate, D3d11Renderer, D3d11VideoCompositor, D3d11VideoCompositorHandle,
        D3d11VideoLayerHandle, TeeBuilder, TeeHandle, VideoCompositorOptions, VideoLayer,
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
        ID3D11DeviceContext,
    },
};

use crate::domain::SourceKind;
use crate::snapshots::SceneItemSnapshot;

use crate::engine::source::{self, OpenSource, unsupported_kind};

use super::{BACKGROUND, BackendError};

use crate::settings::RecordingEncoder;

use media_pp::graph::BranchId;

use crate::engine::preview::{PreviewRenderer, PreviewSurface, SharedTarget};
use crate::engine::source::display_capture::{self, CaptureRegistry};

/// The compositor's layer control already offers exactly what a backend must.
pub(in crate::engine) type Layer = D3d11VideoLayerHandle;

pub(in crate::engine) struct Backend {
    pub(in crate::engine) captures: Arc<CaptureRegistry>,
    pub(in crate::engine) device: ID3D11Device,
    /// The one shared immediate context, kept because the encoder a recording
    /// builds has to be on it like everything else here.
    pub(in crate::engine) context: Arc<Mutex<ID3D11DeviceContext>>,
    pub(in crate::engine) size: [u32; 2],
    pub(in crate::engine) compositor: D3d11VideoCompositorHandle,
    pub(in crate::engine) preview: Arc<Pipeline>,
    /// Where a recording branch is attached — see
    /// [`Backend::attach_recording`].
    pub(in crate::engine) tee: TeeHandle,
    /// Which encoders this machine can open, worked out on first ask.
    pub(in crate::engine) encoders: std::sync::OnceLock<Vec<RecordingEncoder>>,
    /// Reached from the UI through [`Backend::set_preview_visible`] — see
    /// [`PreviewSurface`].
    pub(in crate::engine) surface: Arc<PreviewSurface>,
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

        let surface = PreviewSurface::new(context.clone(), shared, drawn_flag);
        let renderer = D3d11Renderer::new(
            "preview-out",
            Box::new(PreviewRenderer::new(device.clone(), Arc::clone(&surface))),
        );

        // Taken back out of the builder below: `Pipeline::new` runs it once,
        // before returning, and the `Tee` it builds is the only way to attach
        // a recording later.
        let mut tee = None;
        let preview = Pipeline::new("preview", compositor, |source, context| {
            // The counting branch is synchronous — it is how the calls stay
            // at the compositor's own rate — so its sink must stay trivial.
            let count_branch = context.branch().to(Box::new(count))?;
            // The Preview must not set the compositor's pace, so the copy and
            // the repaint it asks for happen on this queue's worker, and the
            // queue drops whatever cannot keep up rather than making the
            // compositor wait.
            //
            // What reaches the renderer past the gate is the newest picture,
            // no more often than the Preview's rate, and never one it has
            // already drawn — so a Scene that is not changing costs nothing
            // at all: no copy into the shared texture, and no egui repaint.
            let draw_branch = context
                .branch()
                .queue_with_policy("preview-queue", 1, OverflowPolicy::DropNewest)
                .pipe(ChangeGate::new(
                    "preview-changes",
                    Duration::from_secs_f32(1.0 / preview_fps as f32),
                ))
                .to(Box::new(renderer))?;
            // `build_dynamic` rather than `build`: the recording branch is
            // attached and detached while this is already running, and the
            // handle is the only way back to this `Tee` afterwards.
            let (tee_branch, tee_handle) = TeeBuilder::new("output-tee", context.clone())
                .branch(count_branch)
                .branch(draw_branch)
                .build_dynamic()?;
            context.attach(source, 0, tee_branch)?;
            tee = Some(tee_handle);
            Ok(())
        })?;
        preview.run()?;
        let tee = tee.expect("Pipeline::new runs the builder before returning");
        Ok(Self {
            captures: Arc::new(CaptureRegistry::default()),
            device,
            context: context.clone(),
            size,
            compositor: handle,
            preview,
            tee,
            encoders: std::sync::OnceLock::new(),
            surface,
        })
    }

    /// Whether anyone is looking at the Preview — see [`PreviewSurface`].
    pub(in crate::engine) fn set_preview_visible(&self, visible: bool) {
        self.surface.set_visible(visible);
    }

    pub(in crate::engine) fn stop(&self) {
        self.preview.stop();
    }

    /// Changes the rate the compositor emits at, which is also the rate a
    /// recording is written at.
    ///
    /// Runtime rather than a restart: the compositor's handle takes it, so
    /// the captures feeding it and the Preview reading it are undisturbed.
    /// Refused by the engine while a recording is running — see
    /// `EngineCommand::VideoSettings` — because the file's encoder was
    /// configured for the old rate and the timestamps it is being handed
    /// would change meaning underneath it.
    pub(in crate::engine) fn set_frame_rate(&self, fps: u32) -> bool {
        // The captures too, not just the compositor. Each paces itself, so
        // one left at 60 behind a compositor at 30 duplicates the desktop
        // twice as often as anything composites it, and one left at 30
        // behind a compositor at 60 leaves half its ticks re-emitting the
        // picture it already had.
        self.captures.set_frame_rate(fps);
        self.compositor
            .set_frame_rate(ffmpeg::Rational::new(fps as i32, 1))
    }

    /// What the compositor is actually emitting at, which is what a recording
    /// has to be configured for.
    ///
    /// Read from the compositor rather than from the setting that asked for
    /// it: a rate it refused leaves the old one running, and a recording
    /// opened for a rate nothing is producing writes a file that claims more
    /// frames a second than it holds.
    pub(in crate::engine) fn frame_rate(&self) -> u32 {
        self.compositor
            .frame_rate()
            .map_or(TARGET_FPS, |rate| {
                (rate.numerator().max(1) / rate.denominator().max(1)) as u32
            })
            .max(1)
    }

    pub(in crate::engine) fn remove_source(&self, name: &str) {
        self.compositor.remove_source(name);
    }

    pub(in crate::engine) fn open_source(
        &self,
        item: &SceneItemSnapshot,
        layer: VideoLayer,
        fps: u32,
        mixer: Option<&media_pp::elements::MixerHandle>,
    ) -> Result<Option<OpenSource>, BackendError> {
        match item.kind {
            SourceKind::DisplayCapture => display_capture::open(
                &self.device,
                &self.compositor,
                &self.captures,
                item,
                layer,
                fps,
            )
            .map(Some),
            SourceKind::WindowCapture => {
                source::window_capture::open(&self.device, &self.compositor, item, layer, fps)
            }
            SourceKind::MediaFile => {
                source::media_file::open(&self.device, &self.compositor, mixer, item, layer)
            }
            SourceKind::Image => source::image::open(&self.device, &self.compositor, item, layer),
            SourceKind::Color => {
                source::color::open(&self.device, &self.compositor, item, layer).map(Some)
            }
            SourceKind::Drawing => {
                source::drawing::open(&self.device, &self.compositor, item, layer).map(Some)
            }
            _ => Err(unsupported_kind(item)),
        }
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

/// One SceneItem's share of whatever is producing its frames.
///
/// Not a `Pipeline`, because one Source is not always one pipeline: a display
/// capture is shared between every item showing that display (see
/// [`capture`]), so stopping one item has to leave the capture running for
/// the others.
pub(in crate::engine) enum RunningSource {
    /// A pipeline this item alone owns, such as a Color Source's pusher.
    Owned(Arc<Pipeline>),
    /// One branch of a display capture other items may also be drawing from.
    Shared {
        captures: Arc<CaptureRegistry>,
        monitor: String,
        branch: BranchId,
    },
}

impl RunningSource {
    pub(in crate::engine) fn pause(&self) {
        match self {
            Self::Owned(pipeline) => pipeline.pause(),
            Self::Shared {
                captures, monitor, ..
            } => captures.set_showing(monitor, false),
        }
    }

    pub(in crate::engine) fn resume(&self) {
        match self {
            Self::Owned(pipeline) => pipeline.resume(),
            Self::Shared {
                captures, monitor, ..
            } => captures.set_showing(monitor, true),
        }
    }

    /// Whether whatever this was capturing has ended by itself.
    ///
    /// See [`super::pipeline_ended`] for why the pipeline is asked rather
    /// than its bus read.
    pub(in crate::engine) fn ended(&self) -> bool {
        match self {
            Self::Owned(pipeline) => super::pipeline_ended(pipeline),
            // A display is not something that closes, and the capture behind
            // one is shared: whether it ended is the registry's business, not
            // one item's.
            Self::Shared { .. } => false,
        }
    }

    pub(in crate::engine) fn stop(&self) {
        match self {
            Self::Owned(pipeline) => pipeline.stop(),
            Self::Shared {
                captures,
                monitor,
                branch,
            } => captures.detach(monitor, *branch),
        }
    }
}
