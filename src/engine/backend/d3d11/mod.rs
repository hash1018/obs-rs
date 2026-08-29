//! The D3D11 backend: DXGI desktop duplication straight into D3D11 textures,
//! a BGRA compositor, and a shared texture to reach wgpu without a readback.

mod capture;
mod shared;

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use eframe::egui;
use eframe::egui_wgpu::RenderState;
use media_pp::{
    buffer::MediaBuffer,
    elements::{
        AppSink, ChangeGate, D3d11Download, D3d11FrameRenderer, D3d11Renderer, D3d11VideoCodec,
        D3d11VideoCompositor, D3d11VideoCompositorHandle, D3d11VideoCompositorInput,
        D3d11VideoEncoder, D3d11VideoEncoderOptions, D3d11VideoInputFormat, D3d11VideoLayerHandle,
        FrameRateLimiter, PauseGate, SubmitError, SwEncoder, SwEncoderOptions, SwScaler,
        TeeBuilder, TeeHandle, TimestampOrigin, VideoCompositorOptions, VideoLayer,
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

use super::{
    BACKGROUND, BackendError, OpenSource, PROBE_FPS, RECORDING_QUEUE_DEPTH, RECORDING_SEND_TIMEOUT,
    VideoTrack, flat_bgra, input_name, software_codec, unsupported_kind,
};

use crate::settings::{RecordingEncoder, RecordingSettings};

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
    /// What the file's video track is stamped in, and what the branch's own
    /// limiter is built against.
    time_base: ffmpeg::Rational,
    /// The rate frames actually reach the encoder at, which is the
    /// compositor's unless the settings asked for less.
    recorded_fps: u32,
    /// The compositor's own rate, kept to decide whether a limiter is needed
    /// at all.
    source_fps: u32,
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
    Hardware(D3d11VideoEncoder),
    /// Needs them copied back from the GPU and converted first.
    Software(SwEncoder),
}

use media_pp::graph::BranchId;

use capture::CaptureRegistry;
use shared::SharedTarget;

/// The compositor's layer control already offers exactly what a backend must.
pub(in crate::engine) type Layer = D3d11VideoLayerHandle;

pub(in crate::engine) struct Backend {
    captures: Arc<CaptureRegistry>,
    device: ID3D11Device,
    /// The one shared immediate context, kept because the encoder a recording
    /// builds has to be on it like everything else here.
    context: Arc<Mutex<ID3D11DeviceContext>>,
    size: [u32; 2],
    compositor: D3d11VideoCompositorHandle,
    preview: Arc<Pipeline>,
    /// Where a recording branch is attached — see
    /// [`Backend::attach_recording`].
    tee: TeeHandle,
    /// Which encoders this machine can open, worked out on first ask.
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

        let surface = Arc::new(PreviewSurface {
            context: context.clone(),
            shared,
            drawn: drawn_flag,
            // Whether the window is up is the UI's to say, and it says so on
            // its next pass; starting visible is what keeps the first frames
            // from being held back until it does.
            visible: AtomicBool::new(true),
            pending: Mutex::new(None),
        });
        let renderer = D3d11Renderer::new(
            "preview-out",
            Box::new(PreviewRenderer {
                device: device.clone(),
                surface: Arc::clone(&surface),
            }),
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
        // `fps` is what the compositor produces; the file is written at what
        // the settings ask for, which can be less but never more.
        let recorded_fps = settings.fps_within(fps);
        Ok(PreparedRecording {
            encoder: self.open_encoder(recorded_fps, settings)?,
            time_base: ffmpeg::Rational::new(1, recorded_fps as i32),
            recorded_fps,
            source_fps: fps,
        })
    }

    /// Builds the recording's video branch onto the compositor's `Tee` and
    /// starts it writing into `sink`.
    ///
    /// Separate from [`Backend::prepare_recording`] only because the sink
    /// cannot exist until every track has been declared — see
    /// [`PreparedRecording`].
    ///
    /// No colour conversion anywhere: the compositor draws BGRA and NVENC
    /// takes BGRA directly, converting to its own YUV as part of encoding.
    ///
    /// # What the queue's policy has to be
    ///
    /// Not the Preview's `DropNewest` — a dropped frame there is one stale
    /// repaint, here it is a frame missing from the file. Not an unbounded
    /// wait either: an encoder that stops answering would then wedge the
    /// compositor, and with it the Preview and every other branch. So it
    /// blocks, but only for a bounded time, and a timeout arrives on the bus
    /// as an error naming this branch rather than as silence.
    pub(in crate::engine) fn attach_recording(
        &self,
        prepared: PreparedRecording,
        sink: Box<dyn media_pp::element::Sink>,
    ) -> Result<VideoTrack, BackendError> {
        let PreparedRecording {
            encoder,
            recorded_fps,
            source_fps: fps,
            ..
        } = prepared;
        let [width, height] = self.size;

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
        // has to reason about it — including a limiter, whose own spacing
        // would otherwise be measured across the gap.
        let (gate, pause) = PauseGate::new("record-pause");
        branch = branch.pipe(gate);
        // The limiter only when it has something to do. At the compositor's
        // own rate it would forward every frame and re-stamp each one to the
        // number it already had; `TimestampOrigin` after the encoder is what
        // moves that timeline to zero instead.
        if recorded_fps < fps {
            branch = branch.pipe(FrameRateLimiter::new(
                "record-rate",
                ffmpeg::Rational::new(1, fps as i32),
                ffmpeg::Rational::new(recorded_fps as i32, 1),
            ));
        }
        branch = match encoder {
            RecordEncoder::Hardware(encoder) => branch.pipe(encoder),
            // A software encoder is not on the GPU and does not take BGRA, so
            // the frames have to come back across the bus and be converted
            // before it sees them. That is the cost the choice carries, and it
            // is why the hardware path is the default.
            RecordEncoder::Software(encoder) => branch
                .pipe(D3d11Download::new(
                    "record-download",
                    &self.device,
                    Arc::clone(&self.context),
                    width,
                    height,
                )?)
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
        let [width, height] = self.size;
        let time_base = ffmpeg::Rational::new(1, fps as i32);
        let frame_rate = ffmpeg::Rational::new(fps as i32, 1);
        let bit_rate = settings.bit_rate_bits();
        let gop_size = fps * settings.keyframe_seconds_clamped();
        match settings.encoder {
            RecordingEncoder::Nvenc | RecordingEncoder::MediaFoundation => {
                Ok(RecordEncoder::Hardware(D3d11VideoEncoder::new(
                    "record-encode",
                    &self.device,
                    Arc::clone(&self.context),
                    D3d11VideoEncoderOptions {
                        codec: if settings.encoder == RecordingEncoder::Nvenc {
                            D3d11VideoCodec::H264Nvenc
                        } else {
                            D3d11VideoCodec::H264MediaFoundation
                        },
                        // The compositor's own output, so neither hardware
                        // path converts anything: both take BGRA directly.
                        input_format: D3d11VideoInputFormat::Bgra,
                        width,
                        height,
                        time_base,
                        frame_rate,
                        bit_rate,
                        gop_size,
                    },
                )?))
            }
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

    /// Which H.264 encoders this machine can actually open — see the CUDA
    /// backend's own copy for why this is probed rather than assumed.
    pub(in crate::engine) fn available_encoders(&self) -> &[RecordingEncoder] {
        self.encoders.get_or_init(|| {
            RecordingEncoder::ALL
                .into_iter()
                .filter(|encoder| {
                    let probe = RecordingSettings {
                        encoder: *encoder,
                        ..RecordingSettings::default()
                    };
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
            SourceKind::DisplayCapture => open_display_capture(
                &self.device,
                &self.compositor,
                &self.captures,
                item,
                layer,
                fps,
            ),
            SourceKind::Color => open_color_source(&self.device, &self.compositor, item, layer),
            _ => Err(unsupported_kind(item)),
        }
    }
}

/// The texture the Preview is drawn from, and whether anyone is looking at
/// it.
///
/// Shared between the renderer inside the pipeline and the `Backend` the UI
/// reaches, because the two answer different halves of one question. A
/// minimised window is nobody looking, and copying a composited frame into a
/// texture no one will sample is 8 MiB a frame spent on nothing.
///
/// What arrives while nobody is looking is kept rather than dropped, and
/// copied the moment the window comes back. Without that the Preview would
/// show the picture from when it was minimised until something on the
/// captured screen happened to change — the `ChangeGate` in front of this
/// forwards changes, and a Scene that is not changing sends nothing at all.
struct PreviewSurface {
    context: Arc<Mutex<ID3D11DeviceContext>>,
    shared: SharedTarget,
    /// Set when the shared texture has new content the Preview has not been
    /// told about; the counting sink clears it as it reports.
    drawn: Arc<AtomicBool>,
    visible: AtomicBool,
    /// The last frame that arrived while nobody was looking.
    ///
    /// Only the texture, not the frame that owned it: the compositor may
    /// compose into it again before the window comes back, and that is not a
    /// problem worth holding a pool frame to prevent — what a restored
    /// Preview should show is the picture as it is then, and drawing over
    /// this one is how it becomes that.
    pending: Mutex<Option<PendingFrame>>,
}

struct PendingFrame {
    texture: ID3D11Texture2D,
    width: u32,
    height: u32,
}

// SAFETY: the COM handles here are `windows-rs` interface wrappers, thread-safe
// to hold; every context call goes through `context`'s own mutex, and the rest
// is plain data behind its own locks.
unsafe impl Send for PreviewSurface {}
unsafe impl Sync for PreviewSurface {}

impl PreviewSurface {
    /// Takes one composited frame, copying it only if there is anyone to see
    /// it. Returns whether the frame was accepted; a copy that fails is the
    /// only rejection.
    fn submit(&self, texture: ID3D11Texture2D, width: u32, height: u32) -> bool {
        if !self.visible.load(Ordering::Relaxed) {
            *self
                .pending
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(PendingFrame {
                texture,
                width,
                height,
            });
            return true;
        }
        self.copy(&texture, width, height)
    }

    /// Tells this whether anyone is looking. Coming back into view copies
    /// whatever arrived while nobody was.
    fn set_visible(&self, visible: bool) {
        self.visible.store(visible, Ordering::Relaxed);
        if !visible {
            return;
        }
        let pending = self
            .pending
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .take();
        if let Some(frame) = pending {
            self.copy(&frame.texture, frame.width, frame.height);
        }
    }

    fn copy(&self, texture: &ID3D11Texture2D, width: u32, height: u32) -> bool {
        if !self.shared.copy_from(&self.context, texture, width, height) {
            return false;
        }
        self.drawn.store(true, Ordering::Relaxed);
        true
    }
}

/// Puts each composited frame it is given into the texture wgpu shares.
///
/// A `D3d11FrameRenderer` normally presents to a window; this one presents to
/// egui, which draws the frame itself. `media-pp` still does the useful half:
/// it validates the frame, rejects one from another device, and hands over a
/// texture that is already exactly what has to be copied.
///
/// What it is given is the `ChangeGate`'s business: the newest picture, at
/// most at the Preview's rate, and never one already drawn. Whether it is
/// copied at all is [`PreviewSurface`]'s.
struct PreviewRenderer {
    device: ID3D11Device,
    surface: Arc<PreviewSurface>,
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
        // Everything that arrives is taken. The rate this is held to, and the
        // frames carrying a picture already on screen, are both the
        // `ChangeGate` in front of it — deliberately, since a renderer that
        // dropped frames of its own would make that gate suppress the
        // repeats carrying a change it had just dropped. Whether taking it
        // means copying it is `PreviewSurface`'s answer, not this one's.
        if !self.surface.submit(texture, width, height) {
            return Err(SubmitError::InvalidFrame);
        }
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
        source: RunningSource::Owned(pipeline),
        layer,
        name,
        refreshed_token: None,
        showing: true,
    })
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

/// Points one SceneItem at a display's capture, opening it if this is the
/// first item to want it.
fn open_display_capture(
    device: &ID3D11Device,
    handle: &D3d11VideoCompositorHandle,
    captures: &Arc<CaptureRegistry>,
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

    let name = input_name(item);
    let D3d11VideoCompositorInput { sink, layer } = handle
        .add_source(name.clone(), layer)?
        .ok_or("the compositor is no longer running")?;
    // The capture is shared, so what this item gets is a branch of it. Its
    // own compositor input is still its own: position, size and z-order stay
    // per item even when the pixels behind two of them are the same.
    let branch = captures.attach(monitor, device, fps, sink)?;

    Ok(OpenSource {
        source: RunningSource::Shared {
            captures: Arc::clone(captures),
            monitor: monitor.clone(),
            branch,
        },
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
