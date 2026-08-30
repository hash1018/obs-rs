//! The CUDA backend: PipeWire capture straight into CUDA surfaces, an NV12
//! compositor, and memory both CUDA and Vulkan hold to reach wgpu.

use crate::engine::TARGET_FPS;
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use eframe::egui;
use eframe::egui_wgpu::RenderState;
use media_pp::{
    buffer::MediaBuffer,
    elements::{
        AppSink, ChangeGate, CudaDevice, CudaRenderer, CudaVideoCompositor,
        CudaVideoCompositorHandle, CudaVideoLayerHandle, TeeBuilder, TeeHandle,
        VideoCompositorOptions, VideoLayer,
    },
    ffmpeg,
    pipeline::Pipeline,
    queue::OverflowPolicy,
    rate::FrameRateHandle,
};

use crate::domain::SourceKind;
use crate::settings::RecordingEncoder;
use crate::snapshots::SceneItemSnapshot;

use crate::engine::source::{self, OpenSource, unsupported_kind};

use super::{BACKGROUND, BackendError};

/// The running recording: which branch it is, and the control that stops it
/// taking frames without stopping anything else.
use crate::engine::preview::{Nv12Target, PreviewRenderer, PreviewSurface, SharedNv12};

/// The compositor's layer control already offers exactly what a backend must.
pub(in crate::engine) type Layer = CudaVideoLayerHandle;

pub(in crate::engine) struct Backend {
    pub(in crate::engine) device: CudaDevice,
    pub(in crate::engine) size: [u32; 2],
    pub(in crate::engine) compositor: CudaVideoCompositorHandle,
    /// Every open capture's rate control, keyed by the name it was
    /// registered with. Held here rather than beside the `OpenSource` the
    /// engine keeps, so that [`Backend::set_frame_rate`] is the one place a
    /// rate change is applied on either platform — the D3D11 backend reaches
    /// its own through the registry that shares captures between items.
    pub(in crate::engine) capture_rates: Mutex<HashMap<String, FrameRateHandle>>,
    pub(in crate::engine) preview: Arc<Pipeline>,
    /// Where a recording branch is attached — see
    /// [`Backend::attach_recording`].
    pub(in crate::engine) tee: TeeHandle,
    /// Which encoders this machine can open, worked out on first ask — see
    /// [`Backend::available_encoders`].
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

        let surface = PreviewSurface::new(
            render_state.device.clone(),
            render_state.queue.clone(),
            target,
            shared,
            drawn_flag,
        );
        let renderer = CudaRenderer::new(
            "preview-out",
            &device,
            Box::new(PreviewRenderer::new(Arc::clone(&surface))),
        );

        // Taken back out of the builder below: `Pipeline::new` runs it once,
        // before returning, and the `Tee` it builds is the only way to attach
        // a recording later.
        let mut tee = None;
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
            device,
            size,
            capture_rates: Mutex::new(HashMap::new()),
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
        self.capture_rates
            .lock()
            .expect("capture rates poisoned")
            .remove(name);
    }

    /// Tells the compositor and every open capture to emit at `fps`.
    ///
    /// Handle calls rather than reopening anything: a capture left at the old
    /// rate behind a compositor at the new one either duplicates the desktop
    /// for frames nothing composites, or leaves the compositor re-emitting a
    /// picture it already had for half its ticks.
    pub(in crate::engine) fn set_frame_rate(&self, fps: u32) -> bool {
        let rate = ffmpeg::Rational::new(fps as i32, 1);
        for capture in self
            .capture_rates
            .lock()
            .expect("capture rates poisoned")
            .values()
        {
            capture.set(rate);
        }
        self.compositor.set_frame_rate(rate)
    }

    pub(in crate::engine) fn open_source(
        &self,
        item: &SceneItemSnapshot,
        layer: VideoLayer,
        fps: u32,
    ) -> Result<OpenSource, BackendError> {
        match item.kind {
            SourceKind::DisplayCapture => {
                let (source, frame_rate) = source::display_capture::open(
                    &self.device,
                    &self.compositor,
                    item,
                    layer,
                    fps,
                )?;
                // Filed under the compositor registration this capture feeds,
                // which is the same key `remove_source` clears it by.
                self.capture_rates
                    .lock()
                    .expect("capture rates poisoned")
                    .insert(source.name.clone(), frame_rate);
                Ok(source)
            }
            SourceKind::Color => source::color::open(&self.device, &self.compositor, item, layer),
            SourceKind::Drawing => {
                source::drawing::open(&self.device, &self.compositor, item, layer)
            }
            _ => Err(unsupported_kind(item)),
        }
    }
}

/// One SceneItem's share of whatever is producing its frames.
///
/// Every Source here owns its own pipeline: the portal hands out a separate
/// stream per request, so unlike Windows' desktop duplication there is
/// nothing two SceneItems *have* to share. See `engine::backend`'s own docs on
/// why this is a type each backend defines rather than a `Pipeline`.
///
/// # Sharing one capture between them was measured, and declined
///
/// Not sharing does cost something. Two SceneItems showing one monitor open
/// two portal sessions, two DMA-BUF imports, two CUDA copies and two
/// `CudaConverter` passes for identical pixels. Measured 2026-08-29 with
/// capture and conversion alone, against a moving screen: one capture cost
/// 3.1% of a core and two cost 6.6%, so the duplicate is about **3.5% of a
/// core** — roughly what an idle Scene costs in total. It is per-buffer work,
/// not per-pixel, so it is the same 3.5% whether a little of the screen is
/// moving or a lot; only a wholly still screen is free, and the repeat
/// handling is what makes it so.
///
/// It is declined anyway, because what it would cost is worse than what it
/// saves:
///
/// - **Window captures could never join.** The portal reports `position()`,
///   the only thing that identifies one source as another, for monitor
///   streams only. Two items on one window would stay duplicated regardless.
/// - **The second dialog cannot be avoided.** Which monitor a session got is
///   known only once the handshake has finished, so a new source is already
///   open — and already prompted — before it can be recognised as a duplicate.
///   Only the pipeline behind it could be collapsed.
/// - **A stale position can share the wrong display.** `position()` is read
///   once, at open. Rearranging monitors afterwards can put a different one
///   where a live capture's key says it is, and the next item to open there
///   would silently be handed the wrong screen.
///
/// Windows had no such choice: `DuplicateOutput` refuses the same output
/// twice, so the second item was a black rectangle until it shared. Here both
/// captures work, and the trade is a few percent of one core against a
/// correctness hazard, in a configuration that is rare to begin with.
pub(in crate::engine) struct RunningSource(
    /// Visible to the rest of the engine because the modules that open a
    /// Source construct one — the Windows half is an enum whose variants are
    /// reachable for the same reason.
    pub(in crate::engine) Arc<Pipeline>,
);

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
