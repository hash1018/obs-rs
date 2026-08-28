//! The CUDA backend: PipeWire capture straight into CUDA surfaces, an NV12
//! compositor, and memory both CUDA and Vulkan hold to reach wgpu.

mod nv12;
mod shared;

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use eframe::egui;
use eframe::egui_wgpu::RenderState;
use media_pp::{
    buffer::MediaBuffer,
    elements::{
        AppSink, ChangeGate, CudaDevice, CudaFrameRenderer, CudaRenderer, CudaVideoCompositor,
        CudaVideoCompositorHandle, CudaVideoLayerHandle, SubmitError, TeeBuilder,
        VideoCompositorOptions, VideoLayer,
    },
    ffmpeg,
    pipeline::Pipeline,
    queue::OverflowPolicy,
};

use crate::domain::{SourceKind, SourceSettings};
use crate::snapshots::SceneItemSnapshot;

use super::{BACKGROUND, BackendError, OpenSource, flat_bgra, input_name, unsupported_kind};

use nv12::Nv12Target;
use shared::SharedNv12;

/// The compositor's layer control already offers exactly what a backend must.
pub(in crate::engine) type Layer = CudaVideoLayerHandle;

pub(in crate::engine) struct Backend {
    device: CudaDevice,
    compositor: CudaVideoCompositorHandle,
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

        // One per process, not one per pipeline: creating or dropping a device
        // while another thread is encoding can fault inside the NVIDIA driver.
        let device = CudaDevice::new()?;

        let target = Nv12Target::new(&render_state.device, width, height);
        let texture_id = render_state.renderer.write().register_native_texture(
            &render_state.device,
            target.output_view(),
            wgpu::FilterMode::Linear,
        );

        let (compositor, handle) = CudaVideoCompositor::new(
            "preview-compositor",
            &device,
            VideoCompositorOptions {
                width,
                height,
                frame_rate: ffmpeg::Rational::new(fps as i32, 1),
                background: BACKGROUND,
            },
        )?;

        // The composited frame reaches wgpu through memory both APIs hold
        // rather than a readback: see `shared`. What it replaces —
        // `CudaDownload` plus `write_texture` — carried every Preview frame
        // across PCIe twice for pixels that never left the GPU.
        let shared = SharedNv12::new(&render_state.device, width, height)?;

        // `on_frame` is not called from the drawing branch: that branch sits
        // behind a dropping queue and refreshes only at the Preview's rate,
        // while the rate reported has to be the compositor's — it is what
        // says whether an output could be made at the rate it is configured
        // for. So the frames are teed. A synchronous counting sink sees every
        // composited frame and makes every call; the drawing branch only
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

        let renderer = CudaRenderer::new(
            "preview-out",
            &device,
            Box::new(PreviewRenderer {
                wgpu_device: render_state.device.clone(),
                queue: render_state.queue.clone(),
                target,
                shared,
                drawn_flag,
            }),
        );

        let preview = Pipeline::new("preview", compositor, |source, context| {
            // The counting branch is synchronous — it is how the calls stay at
            // the compositor's own rate — so its sink must stay trivial.
            let count_branch = context.branch().to(Box::new(count))?;
            // The Preview must not set the compositor's pace, so the copy and
            // the repaint it asks for happen on this queue's worker, and the
            // queue drops whatever cannot keep up rather than making the
            // compositor wait.
            //
            // What reaches the renderer past the gate is the newest picture,
            // no more often than the Preview's rate, and never one it has
            // already drawn — so a Scene that is not changing costs nothing
            // at all: no copy, no resolve pass, and no egui repaint.
            let draw_branch = context
                .branch()
                .queue_with_policy("preview-queue", 1, OverflowPolicy::DropNewest)
                .pipe(ChangeGate::new(
                    "preview-changes",
                    Duration::from_secs_f32(1.0 / preview_fps as f32),
                ))
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

/// Puts each composited frame into the memory wgpu reads, at the Preview's
/// own rate.
///
/// A `CudaFrameRenderer` normally presents to a window; this one presents to
/// egui, which draws the frame itself. `media-pp` still does the useful half:
/// it validates the frame, rejects one belonging to another CUDA context, and
/// hands over exactly the plane pointers a copy needs.
struct PreviewRenderer {
    wgpu_device: wgpu::Device,
    queue: wgpu::Queue,
    target: Nv12Target,
    shared: SharedNv12,
    /// Set when the shared memory has new content the Preview has not been
    /// told about; the counting sink clears it as it reports.
    drawn_flag: Arc<AtomicBool>,
}

impl CudaFrameRenderer for PreviewRenderer {
    unsafe fn submit_nv12(
        &self,
        y: *const u8,
        y_pitch: usize,
        uv: *const u8,
        uv_pitch: usize,
        width: u32,
        height: u32,
    ) -> Result<(), SubmitError> {
        // Everything that arrives is drawn. The rate this is held to, and
        // the frames carrying a picture already on screen, are both the
        // `ChangeGate` in front of it — deliberately, since a renderer that
        // dropped frames of its own would make that gate suppress the
        // repeats carrying a change it had just dropped.
        //
        // SAFETY: `CudaRenderer` has already established what this needs —
        // an NV12 CUDA frame on the primary context, both planes present —
        // before calling, which is the whole reason the element is in the
        // graph rather than an `AppSink`.
        if !unsafe { self.shared.write(y, y_pitch, uv, uv_pitch, width, height) } {
            return Err(SubmitError::InvalidFrame);
        }
        // Checked after the copy rather than before: the frame is only
        // readable at all once it is in memory the CPU can see.
        if self.shared.tail_is_unwritten() {
            return Ok(());
        }
        if !self
            .target
            .draw(&self.wgpu_device, &self.queue, &self.shared)
        {
            return Err(SubmitError::InvalidFrame);
        }
        self.drawn_flag.store(true, Ordering::Relaxed);
        Ok(())
    }

    /// Nothing resizes: the Preview draws the Canvas, whose size the
    /// compositor is built for and does not change while it runs.
    fn resize(&self, _width: u32, _height: u32) -> Result<(), SubmitError> {
        Ok(())
    }
}

/// One SceneItem's share of whatever is producing its frames.
///
/// Every Source here owns its own pipeline: the portal hands out a separate
/// stream per request, so unlike Windows' desktop duplication there is
/// nothing two SceneItems have to share. See `engine::backend`'s own docs on
/// why this is a type each backend defines rather than a `Pipeline`.
pub(in crate::engine) struct RunningSource(Arc<Pipeline>);

impl RunningSource {
    pub(in crate::engine) fn pause(&self) {
        self.0.pause();
    }

    pub(in crate::engine) fn resume(&self) {
        self.0.resume();
    }

    pub(in crate::engine) fn stop(&self) {
        self.0.stop();
    }
}

/// Feeds the compositor one frame of flat colour and leaves it there.
///
/// Pushed once rather than per frame: the compositor keeps the latest frame
/// each input gave it, and a colour that never changes never needs another.
/// Position, size and opacity are the layer's, so nothing here is redrawn
/// when the item moves.
fn open_color_source(
    device: &CudaDevice,
    handle: &CudaVideoCompositorHandle,
    item: &SceneItemSnapshot,
    layer: VideoLayer,
) -> Result<OpenSource, BackendError> {
    use media_pp::elements::{
        AppSource, CudaConverter, CudaFrameFormat, CudaUpload, CudaVideoCompositorInput,
    };

    let SourceSettings::Color(settings) = &item.settings else {
        return Err("scene item is not a color source".into());
    };
    let width = (settings.size[0].round() as u32).max(2) & !1;
    let height = (settings.size[1].round() as u32).max(2) & !1;

    let name = input_name(item);
    let (source, pusher) = AppSource::new(name.clone(), 1);
    // BGRA in, so `CudaConverter` performs the RGB-to-BT.709 conversion the
    // compositor expects instead of this having its own copy of that matrix.
    let upload = CudaUpload::new(
        format!("{name}-upload"),
        device,
        CudaFrameFormat::Bgra,
        width,
        height,
    )?;
    let converter = CudaConverter::new(format!("{name}-convert"), device, width, height)?;

    let CudaVideoCompositorInput { sink, layer } = handle.add_source(name.clone(), layer)?;
    let pipeline = Pipeline::new(name.clone(), source, move |source, context| {
        let branch = context.branch().pipe(upload).pipe(converter).to(sink)?;
        context.attach(source, 0, branch)?;
        Ok(())
    })?;
    pipeline.run()?;
    pusher.push(flat_bgra(width, height, settings.rgba))?;

    Ok(OpenSource {
        source: RunningSource(pipeline),
        layer,
        name,
        refreshed_token: None,
        showing: true,
    })
}

/// Opens the portal's screen cast and wires it into the compositor.
fn open_display_capture(
    device: &CudaDevice,
    handle: &CudaVideoCompositorHandle,
    item: &SceneItemSnapshot,
    layer: VideoLayer,
    fps: u32,
) -> Result<OpenSource, BackendError> {
    use media_pp::elements::{
        CaptureSourceKind, CudaConverter, CudaVideoCompositorInput, PipeWireScreenCaptureOptions,
        PipeWireScreenCaptureSource,
    };

    use crate::domain::{DisplayCaptureTarget, SourceSettings};

    let SourceSettings::DisplayCapture(settings) = &item.settings else {
        return Err("scene item is not a display capture".into());
    };
    let restore_token = match &settings.target {
        DisplayCaptureTarget::Portal { restore_token } => restore_token.clone(),
        // An X11 display name means nothing to the portal, which owns the
        // choice on Wayland. Leaving the token unset makes it prompt, the only
        // thing it can do with a target it cannot resolve.
        DisplayCaptureTarget::MonitorName(_) => None,
    };

    let name = input_name(item);
    // Blocking, and it can sit here indefinitely: an unrecognised token makes
    // the portal show its dialog and wait for the user. Sources are opened one
    // at a time, so the rest wait behind whichever one is asking.
    // GPU capture: the desktop lands in CUDA surfaces and never reaches system
    // memory. It negotiates DMA-BUF only and fails rather than falling back,
    // which is the point — a silent CPU path would undo the whole arrangement.
    let (source, format, refreshed_token) = PipeWireScreenCaptureSource::open_gpu(
        name.clone(),
        PipeWireScreenCaptureOptions {
            fps,
            source_kind: CaptureSourceKind::Monitor,
            include_cursor: false,
            restore_token: restore_token.clone(),
        },
        device,
    )?;
    // A compositor may issue a fresh token on every restore, and keeping the
    // old one then means prompting on every launch — the thing persisting it
    // was for. But declining to issue a new one is not the same as revoking
    // the old, so `None` here must never replace a token that worked: that
    // would throw away the only thing that can reopen this display.
    let refreshed_token = refreshed_token
        .filter(|token| Some(token) != restore_token.as_ref())
        .map(|token| {
            eprintln!("\"{}\": the portal issued a new restore token", item.name);
            Some(token)
        });
    eprintln!(
        "\"{}\": opened {}x{} (token {})",
        item.name,
        format.width,
        format.height,
        if restore_token.is_some() {
            "restored"
        } else {
            "picked"
        }
    );

    // Capture gives BGRA and the compositor works in NV12; nothing between
    // them converts, so this element is not optional.
    let converter = CudaConverter::new(
        format!("{name}-convert"),
        device,
        format.width,
        format.height,
    )?;

    let CudaVideoCompositorInput { sink, layer } = handle.add_source(name.clone(), layer)?;
    let pipeline = Pipeline::new(name.clone(), source, move |source, context| {
        let branch = context.branch().pipe(converter).to(sink)?;
        context.attach(source, 0, branch)?;
        Ok(())
    })?;
    pipeline.run()?;

    Ok(OpenSource {
        source: RunningSource(pipeline),
        layer,
        name,
        refreshed_token,
        showing: true,
    })
}
