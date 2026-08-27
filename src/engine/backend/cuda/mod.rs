//! The CUDA backend: PipeWire capture straight into CUDA surfaces, an NV12
//! compositor, and one download to reach wgpu.

mod nv12;

use std::sync::Arc;
use std::time::{Duration, Instant};

use eframe::egui;
use eframe::egui_wgpu::RenderState;
use media_pp::{
    buffer::MediaBuffer,
    elements::{
        AppSink, CudaDevice, CudaDownload, CudaFrameFormat, CudaVideoCompositor,
        CudaVideoCompositorHandle, CudaVideoLayerHandle, VideoCompositorOptions, VideoLayer,
    },
    ffmpeg,
    pipeline::Pipeline,
    queue::OverflowPolicy,
};

use crate::domain::{SourceKind, SourceSettings};
use crate::snapshots::SceneItemSnapshot;

use super::{BACKGROUND, BackendError, OpenSource, flat_bgra, input_name, unsupported_kind};

use nv12::Nv12Target;

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

        // The compositor works in CUDA surfaces; this is the one place the
        // frame returns to system memory. Replacing this download with an
        // import into the planes above is what removes the round trip.
        let download = CudaDownload::new(
            "preview-download",
            &device,
            CudaFrameFormat::Nv12,
            width,
            height,
        );

        // The sink runs on the compositor's own source thread, so neither the
        // UI thread nor the engine's does the conversion.
        let wgpu_device = render_state.device.clone();
        let queue = render_state.queue.clone();
        let interval = Duration::from_secs_f32(1.0 / preview_fps as f32);
        let mut last_drawn: Option<Instant> = None;
        let sink = AppSink::new("preview-out", move |buffer| {
            let MediaBuffer::Video(video) = buffer else {
                return Ok(());
            };
            // Dropped here rather than upstream: the download is the cheapest
            // part of this branch, while the upload, the resolve pass, and the
            // full egui repaint each drawn frame asks for are not.
            let due = last_drawn.is_none_or(|last| last.elapsed() >= interval);
            let drawn = due && target.draw(&wgpu_device, &queue, &video);
            if drawn {
                last_drawn = Some(Instant::now());
            }
            // Every composited frame, drawn or not: this is the compositor's
            // rate, and it is the one that says whether an output could be
            // made at the rate it is configured for.
            on_frame(drawn.then_some(texture_id));
            Ok(())
        });

        let preview = Pipeline::new("preview", compositor, |source, context| {
            // The Preview must not set the compositor's pace. `CudaDownload`
            // waits for the GPU to finish before the CPU can read, and a
            // synchronous chain makes the compositor wait with it — which
            // dragged a 60 fps compositor down to exactly half that.
            let branch = context
                .branch()
                .queue_with_policy("preview-queue", 1, OverflowPolicy::DropNewest)
                .pipe(download)
                .to(Box::new(sink))?;
            context.attach(source, 0, branch)?;
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
        pipeline,
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
        pipeline,
        layer,
        name,
        refreshed_token,
        showing: true,
    })
}
