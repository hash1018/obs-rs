//! The CUDA backend: PipeWire capture straight into CUDA surfaces, an NV12
//! compositor, and memory both CUDA and Vulkan hold to reach wgpu.

mod nv12;
mod shared;

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
        AppSink, ChangeGate, CudaCodec, CudaDevice, CudaDownload, CudaEncoder, CudaEncoderOptions,
        CudaFrameFormat, CudaFrameRenderer, CudaRenderer, CudaScaler, CudaScalerInterp,
        CudaVideoCompositor, CudaVideoCompositorHandle, CudaVideoLayerHandle, PauseGate,
        SubmitError, SwEncoder, SwEncoderOptions, SwScaler, TeeBuilder, TeeHandle, TimestampOrigin,
        VideoCompositorOptions, VideoLayer,
    },
    ffmpeg,
    pipeline::Pipeline,
    queue::OverflowPolicy,
    rate::FrameRateHandle,
};

use crate::domain::{SourceKind, SourceSettings};
use crate::settings::{RecordingEncoder, RecordingSettings};
use crate::snapshots::SceneItemSnapshot;

use crate::engine::source::{self, OpenSource, input_name, unsupported_kind};

use super::{
    BACKGROUND, BackendError, PROBE_FPS, RECORDING_QUEUE_DEPTH, RECORDING_SEND_TIMEOUT, VideoTrack,
    software_codec,
};

/// The running recording: which branch it is, and the control that stops it
/// taking frames without stopping anything else.
/// A video encoder opened and ready, waiting only for the muxer sink it
/// writes into.
///
/// It exists because an mp4's tracks are fixed before its header is written,
/// and the audio track is added by `engine::recording` — which cannot open
/// this one, since which encoder and which frame format are the backend's
/// own. So the work splits: this end opens the encoder and says what stream
/// it needs, and the branch is built once the sink for it exists.
pub(in crate::engine) struct PreparedRecording {
    encoder: RecordEncoder,
    /// What the file's video track is stamped in — the reciprocal of the
    /// rate the compositor is running at, which is the only rate frames can
    /// arrive at.
    time_base: ffmpeg::Rational,
    /// What the file is written at, which is the Scene Canvas unless the
    /// settings asked for less. The encoder was opened for it, so the branch
    /// has to deliver it.
    size: [u32; 2],
}

impl PreparedRecording {
    /// What `Mp4Muxer::add_stream` needs to describe this track.
    pub(in crate::engine) fn parameters(&self) -> ffmpeg::codec::Parameters {
        match &self.encoder {
            RecordEncoder::Hardware(encoder) => encoder.parameters(),
            RecordEncoder::Software(encoder) => encoder.parameters(),
        }
    }

    pub(in crate::engine) fn time_base(&self) -> ffmpeg::Rational {
        self.time_base
    }
}

/// One opened encoder, and which kind of chain it needs in front of it.
enum RecordEncoder {
    /// Takes the compositor's frames as they are.
    Hardware(CudaEncoder),
    /// Needs them copied back from the GPU and converted first.
    Software(SwEncoder),
}

use nv12::Nv12Target;
use shared::SharedNv12;

/// The compositor's layer control already offers exactly what a backend must.
pub(in crate::engine) type Layer = CudaVideoLayerHandle;

pub(in crate::engine) struct Backend {
    device: CudaDevice,
    size: [u32; 2],
    compositor: CudaVideoCompositorHandle,
    /// Every open capture's rate control, keyed by the name it was
    /// registered with. Held here rather than beside the `OpenSource` the
    /// engine keeps, so that [`Backend::set_frame_rate`] is the one place a
    /// rate change is applied on either platform — the D3D11 backend reaches
    /// its own through the registry that shares captures between items.
    capture_rates: Mutex<HashMap<String, FrameRateHandle>>,
    preview: Arc<Pipeline>,
    /// Where a recording branch is attached — see
    /// [`Backend::attach_recording`].
    tee: TeeHandle,
    /// Which encoders this machine can open, worked out on first ask — see
    /// [`Backend::available_encoders`].
    encoders: std::sync::OnceLock<Vec<RecordingEncoder>>,
    /// Reached from the UI through [`Backend::set_preview_visible`] — see
    /// [`PreviewSurface`].
    surface: Arc<PreviewSurface>,
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

        let surface = Arc::new(PreviewSurface {
            wgpu_device: render_state.device.clone(),
            queue: render_state.queue.clone(),
            target,
            shared,
            drawn: drawn_flag,
            // Whether the window is up is the UI's to say, and it says so on
            // its next pass; starting visible is what keeps the first frames
            // from being held back until it does.
            visible: AtomicBool::new(true),
            undrawn: AtomicBool::new(false),
        });
        let renderer = CudaRenderer::new(
            "preview-out",
            &device,
            Box::new(PreviewRenderer {
                surface: Arc::clone(&surface),
            }),
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

    /// Opens the encoder this recording's video track needs, and says what
    /// stream to declare for it.
    ///
    /// Nothing is attached and nothing is written: an mp4's tracks are fixed
    /// before its header is, so the encoder has to exist before the sink it
    /// will write into can — see [`PreparedRecording`], and
    /// [`Backend::attach_recording`] for the half that draws.
    pub(in crate::engine) fn prepare_recording(
        &self,
        fps: u32,
        settings: &crate::settings::RecordingSettings,
    ) -> Result<PreparedRecording, BackendError> {
        // The compositor's own rate, which the settings have already been
        // applied to — a recording is written at what is being composited,
        // and there is nothing in between to re-rate it. Read from the
        // compositor rather than from the setting so that a rate it refused
        // cannot produce a file claiming frames nothing is making.
        Ok(PreparedRecording {
            encoder: self.open_encoder(fps, settings)?,
            time_base: ffmpeg::Rational::new(1, fps as i32),
            size: settings.output_size(self.size),
        })
    }

    /// Builds the recording's video branch onto the compositor's `Tee` and
    /// starts it writing into `sink`.
    ///
    /// Separate from [`Backend::prepare_recording`] only because the sink
    /// cannot exist until every track has been declared — see
    /// [`PreparedRecording`].
    ///
    /// No colour conversion anywhere: the compositor draws NV12 and NVENC
    /// takes NV12 as its own native input.
    ///
    /// # What the queue's policy has to be
    ///
    /// Not the Preview's `DropNewest` — a dropped frame there is one stale
    /// repaint, here it is a frame missing from the file. Not an unbounded
    /// wait either: an encoder that stops answering would then wedge the
    /// compositor, and with it the Preview and every other branch. So it
    /// blocks, but only for a bounded time, and a timeout arrives on the bus
    /// as an error naming this branch rather than as silence.
    ///
    /// # Verified on this backend
    ///
    /// The commits that built this could only reason about the Linux half —
    /// the host they were written on cannot build the CUDA backend — so it is
    /// worth writing down that it was afterwards run. Two live display
    /// captures, 1920x1080 H.264 at 60:
    ///
    /// - 717 frames over 11.95 s, 959 over 15.98 s, 1198 over 19.97 s: 60.0
    ///   throughout, and the compositor held 59.99 of its 60.00 while
    ///   recording. A decoded frame from the middle is the composited Canvas.
    /// - `start_time` is 0.050000 on every file, including a second recording
    ///   started 24 s into the same session — three frames of B-frame reorder
    ///   delay, which is what [`TimestampOrigin`] leaves behind rather than
    ///   the compositor's uptime.
    /// - 1439 frames over 23.98 s across a twelve-second minimise, so the
    ///   Preview going idle takes nothing from this branch.
    /// - A directory it cannot write reaches the status bar as "Recording
    ///   could not start — ffmpeg error: Permission denied", the clock stays
    ///   at `--:--:--`, and the next attempt clears it.
    pub(in crate::engine) fn attach_recording(
        &self,
        prepared: PreparedRecording,
        sink: Box<dyn media_pp::element::Sink>,
    ) -> Result<VideoTrack, BackendError> {
        let PreparedRecording { encoder, size, .. } = prepared;
        let [width, height] = size;

        let mut branch = self
            .tee
            .branch()
            .ok_or("the compositor's Tee is gone")?
            .queue_with_policy(
                "record-queue",
                RECORDING_QUEUE_DEPTH,
                OverflowPolicy::Block(RECORDING_SEND_TIMEOUT),
            );
        // The gate first, so a paused span is gone before anything downstream
        // has to reason about it.
        let (gate, pause) = PauseGate::new("record-pause");
        branch = branch.pipe(gate);
        // Only when the file is smaller than the canvas.
        if size != self.size {
            branch = branch.pipe(CudaScaler::new(
                "record-scale",
                &self.device,
                width,
                height,
                // Downscaling a screen recording is exactly the quality-sensitive
                // path that enum documents: nearest on 1080p text is
                // visibly worse, and this runs on the GPU either way.
                CudaScalerInterp::Lanczos,
            ));
        }
        branch = match encoder {
            RecordEncoder::Hardware(encoder) => branch.pipe(encoder),
            // A software encoder is not on the GPU and does not take NV12, so
            // the frames have to come back across the bus and be converted
            // before it sees them. That is the cost the choice carries, and it
            // is why the hardware path is the default.
            RecordEncoder::Software(encoder) => branch
                .pipe(CudaDownload::new(
                    "record-download",
                    &self.device,
                    CudaFrameFormat::Nv12,
                    width,
                    height,
                ))
                .pipe(SwScaler::new(
                    "record-convert",
                    ffmpeg::format::Pixel::YUV420P,
                    width,
                    height,
                    ffmpeg::software::scaling::Flags::BILINEAR,
                ))
                .pipe(encoder),
        };
        let branch = branch
            // The compositor has been running since the application started, and
            // its timeline says so. Without this the file is written as
            // beginning that far in, and a player shows the lead-in as empty.
            .pipe(TimestampOrigin::new("record-origin"))
            .to(sink)?;
        Ok(VideoTrack {
            branch: self.tee.attach(branch)?,
            pause,
        })
    }

    /// Opens whichever encoder the settings name.
    fn open_encoder(
        &self,
        fps: u32,
        settings: &RecordingSettings,
    ) -> Result<RecordEncoder, BackendError> {
        let [width, height] = settings.output_size(self.size);
        let time_base = ffmpeg::Rational::new(1, fps as i32);
        let frame_rate = ffmpeg::Rational::new(fps as i32, 1);
        let bit_rate = settings.bit_rate_bits();
        let gop_size = fps * settings.keyframe_seconds_clamped();
        match settings.encoder {
            RecordingEncoder::Nvenc => Ok(RecordEncoder::Hardware(CudaEncoder::new(
                "record-encode",
                &self.device,
                CudaEncoderOptions {
                    codec: CudaCodec::H264,
                    input_format: CudaFrameFormat::Nv12,
                    width,
                    height,
                    time_base,
                    frame_rate,
                    bit_rate,
                    gop_size,
                },
            )?)),
            other => Ok(RecordEncoder::Software(SwEncoder::new(
                "record-encode",
                SwEncoderOptions {
                    codec: software_codec(other),
                    width,
                    height,
                    time_base,
                    frame_rate,
                    bit_rate,
                    gop_size,
                },
            )?)),
        }
    }

    /// Which H.264 encoders this machine can actually open, in the order the
    /// list should offer them.
    ///
    /// Probed by opening each one, not by asking whether FFmpeg knows the
    /// name. A build can carry `h264_nvenc` on a machine with no NVIDIA
    /// encoder, and `libx264` is missing from a good many builds — this
    /// machine's included — so neither question is answered by the encoder
    /// list alone.
    ///
    /// Probed once. Opening NVENC is not free, and the answer cannot change
    /// while the application is running.
    pub(in crate::engine) fn available_encoders(&self) -> &[RecordingEncoder] {
        self.encoders.get_or_init(|| {
            RecordingEncoder::ALL
                .into_iter()
                .filter(|encoder| {
                    let probe = RecordingSettings {
                        encoder: *encoder,
                        ..RecordingSettings::default()
                    };
                    // The Canvas's own size, not a token one: an encoder that
                    // opens at 320x240 and refuses 4K would be offered and
                    // then fail at the moment it was used.
                    self.open_encoder(PROBE_FPS, &probe).is_ok()
                })
                .collect()
        })
    }

    /// Ends the recording's video track.
    ///
    /// `finish_branch` rather than `detach`: an mp4 is unplayable until its
    /// trailer is written, and that happens when the muxer sees the branch's
    /// `Eos`. Detaching would drop the branch instead, leaving the file
    /// exactly as long as it is useless. `finish_branch` detaches too, so the
    /// branch id is spent either way.
    ///
    /// Only *this* track: the trailer is written once every track has
    /// reported done, so a file with audio in it stays unplayable until the
    /// audio branch is finished too. Ending both is `engine::recording`'s
    /// job, and the reason it rather than this owns them.
    ///
    /// Returns once the `Eos` is on its way, not once the file is closed: the
    /// encoder flush and the trailer happen on a thread the `Tee` owns, so
    /// this does not block the engine. The file is complete a moment after
    /// this returns rather than at the instant it does.
    pub(in crate::engine) fn detach_recording(
        &self,
        track: VideoTrack,
    ) -> Result<(), BackendError> {
        self.tee.finish_branch(track.branch)?;
        Ok(())
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
                let (source, frame_rate) =
                    open_display_capture(&self.device, &self.compositor, item, layer, fps)?;
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

/// Puts each composited frame into the memory wgpu reads, at the Preview's
/// own rate.
///
/// A `CudaFrameRenderer` normally presents to a window; this one presents to
/// egui, which draws the frame itself. `media-pp` still does the useful half:
/// it validates the frame, rejects one belonging to another CUDA context, and
/// hands over exactly the plane pointers a copy needs.
struct PreviewRenderer {
    surface: Arc<PreviewSurface>,
}

/// What the Preview is drawn from, and whether anyone is looking at it.
///
/// Shared between the renderer inside the pipeline and the `Backend` the UI
/// reaches. A minimised window is nobody looking, and the resolve pass and
/// the buffer-to-texture copies it drives are work for a texture no one will
/// sample.
///
/// The CUDA copy into shared memory still happens: it is device-to-device and
/// cheap, and keeping it current is what lets the window come back to the
/// picture as it is then rather than as it was when it went down — the
/// `ChangeGate` in front of this forwards changes, and a Scene that is not
/// changing sends nothing at all.
struct PreviewSurface {
    wgpu_device: wgpu::Device,
    queue: wgpu::Queue,
    target: Nv12Target,
    shared: SharedNv12,
    /// Set when the shared memory has new content the Preview has not been
    /// told about; the counting sink clears it as it reports.
    drawn: Arc<AtomicBool>,
    visible: AtomicBool,
    /// Whether the shared memory holds a picture the target has not drawn,
    /// which is what coming back into view has to answer.
    undrawn: AtomicBool,
}

impl PreviewSurface {
    /// Draws what is in shared memory, if there is anyone to see it.
    fn present(&self) -> bool {
        if !self.visible.load(Ordering::Relaxed) {
            self.undrawn.store(true, Ordering::Relaxed);
            return true;
        }
        if !self
            .target
            .draw(&self.wgpu_device, &self.queue, &self.shared)
        {
            return false;
        }
        self.drawn.store(true, Ordering::Relaxed);
        true
    }

    /// Tells this whether anyone is looking. Coming back into view draws
    /// whatever arrived while nobody was.
    fn set_visible(&self, visible: bool) {
        self.visible.store(visible, Ordering::Relaxed);
        if visible && self.undrawn.swap(false, Ordering::Relaxed) {
            self.present();
        }
    }
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
        if !unsafe {
            self.surface
                .shared
                .write(y, y_pitch, uv, uv_pitch, width, height)
        } {
            return Err(SubmitError::InvalidFrame);
        }
        // Checked after the copy rather than before: the frame is only
        // readable at all once it is in memory the CPU can see.
        if self.surface.shared.tail_is_unwritten() {
            return Ok(());
        }
        // Whether drawing it happens now is `PreviewSurface`'s answer, not
        // this one's.
        if !self.surface.present() {
            return Err(SubmitError::InvalidFrame);
        }
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

/// Opens the portal's screen cast and wires it into the compositor.
fn open_display_capture(
    device: &CudaDevice,
    handle: &CudaVideoCompositorHandle,
    item: &SceneItemSnapshot,
    layer: VideoLayer,
    fps: u32,
) -> Result<(OpenSource, FrameRateHandle), BackendError> {
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

    // Before the move into the `Pipeline` below, which is the only chance to
    // take it. `open_display_capture` is a free function, so it is handed
    // back and the caller files it — see [`Backend::set_frame_rate`].
    let frame_rate = source.frame_rate();

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

    Ok((
        OpenSource {
            source: RunningSource(pipeline),
            layer,
            name,
            refreshed_token,
            showing: true,
            pushed: None,
        },
        frame_rate,
    ))
}
