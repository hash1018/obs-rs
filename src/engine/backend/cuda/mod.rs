//! The CUDA backend: PipeWire capture straight into CUDA surfaces, an NV12
//! compositor, and memory both CUDA and Vulkan hold to reach wgpu.

mod nv12;
mod shared;

use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use eframe::egui;
use eframe::egui_wgpu::RenderState;
use media_pp::{
    buffer::MediaBuffer,
    elements::{
        AppSink, ChangeGate, CudaCodec, CudaDevice, CudaEncoder, CudaEncoderOptions,
        FrameRateLimiter,
        CudaFrameFormat, CudaFrameRenderer, CudaRenderer, CudaVideoCompositor,
        CudaDownload, CudaVideoCompositorHandle, CudaVideoLayerHandle, Mp4Muxer, SubmitError,
        SwEncoder, SwEncoderOptions, SwScaler, TeeBuilder, TeeHandle, TimestampOrigin,
        VideoCompositorOptions, VideoLayer,
    },
    ffmpeg,
    graph::BranchId,
    pipeline::Pipeline,
    queue::OverflowPolicy,
};

use crate::domain::{SourceKind, SourceSettings};
use crate::settings::{RecordingEncoder, RecordingSettings};
use crate::snapshots::SceneItemSnapshot;

use super::{
    BACKGROUND, BackendError, OpenSource, PROBE_FPS, RECORDING_QUEUE_DEPTH,
    RECORDING_SEND_TIMEOUT, flat_bgra, input_name, software_codec, unsupported_kind,
};

/// One opened encoder, and which kind of chain it needs in front of it.
enum RecordEncoder {
    /// Takes the compositor's frames as they are.
    Hardware(CudaEncoder),
    /// Needs them copied back from the GPU and converted first.
    Software(SwEncoder),
}

impl RecordEncoder {
    /// What the muxer has to be told about the track before its header is
    /// written.
    fn parameters(&self) -> ffmpeg::codec::Parameters {
        match self {
            Self::Hardware(encoder) => encoder.parameters(),
            Self::Software(encoder) => encoder.parameters(),
        }
    }
}

use nv12::Nv12Target;
use shared::SharedNv12;

/// The compositor's layer control already offers exactly what a backend must.
pub(in crate::engine) type Layer = CudaVideoLayerHandle;

pub(in crate::engine) struct Backend {
    device: CudaDevice,
    size: [u32; 2],
    compositor: CudaVideoCompositorHandle,
    preview: Arc<Pipeline>,
    /// Where a recording branch is attached — see [`Backend::start_recording`].
    tee: TeeHandle,
    /// The recording branch while one is running. `Mutex` rather than an
    /// atomic because starting one builds a file and an encoder, and two
    /// concurrent starts must not both get that far.
    recording: Mutex<Option<BranchId>>,
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
            compositor: handle,
            preview,
            tee,
            recording: Mutex::new(None),
            encoders: std::sync::OnceLock::new(),
            surface,
        })
    }

    /// Whether anyone is looking at the Preview — see [`PreviewSurface`].
    pub(in crate::engine) fn set_preview_visible(&self, visible: bool) {
        self.surface.set_visible(visible);
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

    /// Attaches an encode-and-mux branch to the compositor's own `Tee`, so a
    /// recording is made of exactly the frames the Preview is showing.
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
    pub(in crate::engine) fn start_recording(
        &self,
        path: &Path,
        fps: u32,
        settings: &crate::settings::RecordingSettings,
    ) -> Result<(), BackendError> {
        let mut recording = self.recording.lock().expect("recording state poisoned");
        if recording.is_some() {
            return Err("a recording is already running".into());
        }
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }

        let [width, height] = self.size;
        // `fps` is what the compositor produces; the file is written at what
        // the settings ask for, which can be less but never more.
        let recorded_fps = settings.fps_within(fps);
        let time_base = ffmpeg::Rational::new(1, recorded_fps as i32);
        let encoder = self.open_encoder(recorded_fps, settings)?;

        // The file's tracks are fixed before its header is written, which is
        // why audio cannot be added to a recording already running — the same
        // constraint OBS has, and the reason a track list is decided here.
        let mut muxer = Mp4Muxer::create(path)?;
        muxer.add_stream("video", encoder.parameters(), time_base)?;
        let sink = muxer
            .open()?
            .pop()
            .ok_or("the muxer produced no track sink")?;

        let mut branch = self
            .tee
            .branch()
            .ok_or("the compositor's Tee is gone")?
            .queue_with_policy(
                "record-queue",
                RECORDING_QUEUE_DEPTH,
                OverflowPolicy::Block(RECORDING_SEND_TIMEOUT),
            );
        // Only when it has something to do. At the compositor's own rate the
        // limiter would forward every frame and re-stamp each one to the
        // number it already had, and `TimestampOrigin` after the encoder is
        // what moves that timeline to zero instead.
        if recorded_fps < fps {
            branch = branch.pipe(FrameRateLimiter::new(
                "record-rate",
                ffmpeg::Rational::new(1, fps as i32),
                ffmpeg::Rational::new(recorded_fps as i32, 1),
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
        *recording = Some(self.tee.attach(branch)?);
        Ok(())
    }

    /// Opens whichever encoder the settings name.
    fn open_encoder(
        &self,
        fps: u32,
        settings: &RecordingSettings,
    ) -> Result<RecordEncoder, BackendError> {
        let [width, height] = self.size;
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

    /// Ends the recording and finalizes its file.
    ///
    /// `finish_branch` rather than `detach`: an mp4 is unplayable until its
    /// trailer is written, and that happens when the muxer sees the branch's
    /// `Eos`. Detaching would drop the branch instead, leaving the file
    /// exactly as long as it is useless. `finish_branch` detaches too, so the
    /// branch id is spent either way.
    ///
    /// Returns once the `Eos` is on its way, not once the file is closed: the
    /// encoder flush and the trailer happen on a thread the `Tee` owns, so
    /// this does not block the engine. The file is complete a moment after
    /// this returns rather than at the instant it does.
    pub(in crate::engine) fn stop_recording(&self) -> Result<(), BackendError> {
        let mut recording = self.recording.lock().expect("recording state poisoned");
        let Some(branch) = recording.take() else {
            return Err("no recording is running".into());
        };
        self.tee.finish_branch(branch)?;
        Ok(())
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
