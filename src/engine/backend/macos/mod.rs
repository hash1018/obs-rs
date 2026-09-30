//! The macOS backend: an NV12 compositor on Metal, over VideoToolbox frames —
//! see `gpu` — and the frame it hands wgpu.
//!
//! The Linux backend's shape with one GPU in it: the Canvas is NV12, BT.709
//! at limited range, which the VideoToolbox encoder takes as it is, and the
//! Preview resolves it into RGBA with the same pass Linux uses. What reaches
//! that pass does not go through system memory: the compositor's planes are
//! copied on the GPU, into the textures wgpu owns — see `preview`.

mod gpu;

use crate::engine::TARGET_FPS;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use eframe::egui;
use eframe::egui_wgpu::RenderState;
use media_pp::{
    buffer::MediaBuffer,
    elements::{AppSink, ChangeGate, MetalRenderer, TeeHandle, VideoCompositorOptions, VideoLayer},
    ffmpeg,
    pipeline::Pipeline,
    queue::OverflowPolicy,
};

use crate::domain::SourceKind;
use crate::engine::audio::MeterWake;
use crate::engine::source::filters::ChainFormat;
use crate::settings::RecordingEncoder;
use crate::snapshots::SceneItemSnapshot;

use crate::engine::source::shared::{Share, SharedCapture};
use crate::engine::source::{self, OpenOutcome};

use super::{BACKGROUND, BackendError, Target};

use crate::engine::preview::{Nv12Target, PreviewRenderer, PreviewSurface};

pub(in crate::engine) use gpu::{Compositor, Gpu, Layer};

pub(in crate::engine) struct Backend {
    /// The VideoToolbox context every Source's elements are made on — see
    /// [`Gpu`]. Kept for the reason the Linux twin keeps its own: a filter
    /// rack builds elements long after the Source was opened.
    pub(in crate::engine) gpu: Gpu,
    pub(in crate::engine) size: [u32; 2],
    pub(in crate::engine) compositor: Compositor,
    /// Every display and window being captured, each once however many
    /// items show it — see `source::screencapturekit_capture`.
    pub(in crate::engine) screens: Arc<source::screencapturekit_capture::ScreenRegistry>,
    /// The cameras this backend has open, each once however many items show
    /// it — see `source::video_capture`.
    pub(in crate::engine) cameras: Arc<source::video_capture::CameraRegistry>,
    /// Every Scene being composited for another Scene — see
    /// `source::scene`.
    scenes: Arc<source::scene::SceneRegistry>,
    pub(in crate::engine) preview: Arc<Pipeline>,
    /// Where a recording branch is attached — see
    /// [`Backend::attach_output`].
    pub(in crate::engine) tee: TeeHandle,
    /// Which encoders this machine can open, worked out on first ask — see
    /// [`Backend::available_encoders`].
    pub(in crate::engine) encoders: std::sync::OnceLock<Vec<RecordingEncoder>>,
    /// Reached from the UI through [`Backend::set_preview_visible`] — see
    /// [`PreviewSurface`].
    pub(in crate::engine) surface: Arc<PreviewSurface>,
    /// Handed to the meter of every Source that brings its own sound.
    pub(in crate::engine) meter_wake: MeterWake,
}

impl Backend {
    pub(in crate::engine) fn start(
        render_state: &RenderState,
        size: [u32; 2],
        fps: u32,
        preview_fps: u32,
        on_frame: impl Fn(Option<egui::TextureId>) + Send + Sync + 'static,
        meter_wake: MeterWake,
    ) -> Result<Self, BackendError> {
        let [width, height] = size;

        let gpu = Gpu::open()?;
        tracing::info!("compositing with {}", gpu.describe());

        let target = Nv12Target::new(&render_state.device, width, height);
        let texture_id = render_state.renderer.write().register_native_texture(
            &render_state.device,
            target.output_view(),
            wgpu::FilterMode::Linear,
        );

        let (compositor, handle) = gpu.compositor(
            "preview-compositor".to_owned(),
            VideoCompositorOptions {
                mode: media_pp::elements::RenderMode::Live,
                width,
                height,
                frame_rate: ffmpeg::Rational::new(fps as i32, 1),
                background: BACKGROUND,
                background_alpha: 255,
            },
            ChainFormat::Nv12,
        )?;

        // Every composited frame counted, only the drawn ones reported with
        // the texture — see the Linux twin for why the two are teed.
        let drawn_flag = Arc::new(AtomicBool::new(false));
        let count = {
            let drawn_flag = Arc::clone(&drawn_flag);
            AppSink::new("preview-rate", move |buffer| {
                if matches!(buffer, MediaBuffer::Video(_)) {
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
            &render_state.device,
            &render_state.queue,
            target,
            drawn_flag,
        )?;
        let renderer = MetalRenderer::new(
            "preview-out",
            Box::new(PreviewRenderer::new(Arc::clone(&surface))),
        );

        let (preview, tee) = compositor.pipeline("preview".to_owned(), |context| {
            let count_branch = context.branch().to(count)?;
            // Behind a dropping queue and a change gate, as on the other
            // platforms: the Preview never sets the compositor's pace, and a
            // Scene that is not changing costs no copy and no repaint.
            let draw_branch = context
                .branch()
                .queue_with_policy("preview-queue", 1, OverflowPolicy::DropNewest)
                .pipe(ChangeGate::new(
                    "preview-changes",
                    Duration::from_secs_f32(1.0 / preview_fps as f32),
                ))
                .to(renderer)?;
            context
                .tee("output-tee")
                .branch(count_branch)
                .branch(draw_branch)
                .build_dynamic()
        })?;
        preview.run()?;

        Ok(Self {
            gpu,
            size,
            screens: Arc::new(source::screencapturekit_capture::ScreenRegistry::default()),
            cameras: Arc::new(source::video_capture::CameraRegistry::default()),
            scenes: Arc::new(source::scene::SceneRegistry::default()),
            compositor: handle,
            preview,
            tee,
            encoders: std::sync::OnceLock::new(),
            surface,
            meter_wake,
        })
    }

    /// Whether anyone is looking at the Preview — see [`PreviewSurface`].
    pub(in crate::engine) fn set_preview_visible(&self, visible: bool) {
        self.surface.set_visible(visible);
    }

    pub(in crate::engine) fn stop(&self) {
        self.preview.stop();
    }

    /// What the compositor is actually emitting at — see the Linux twin.
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
        self.scenes
            .open
            .each(|composition| composition.extra.remove_source(name));
    }

    /// Tells the compositor and every open capture to emit at `fps` — see
    /// the D3D11 twin, which reaches its captures the same way, through the
    /// registry that shares them.
    pub(in crate::engine) fn set_frame_rate(&self, fps: u32) -> bool {
        self.screens.set_frame_rate(fps);
        self.compositor
            .set_frame_rate(ffmpeg::Rational::new(fps as i32, 1))
            .inspect_err(|error| tracing::warn!("the compositor kept its rate: {error}"))
            .is_ok()
    }

    /// Opens one Scene item's Source, leaving nothing behind if it fails —
    /// see the Linux twin.
    pub(in crate::engine) fn open_source(
        &self,
        item: &SceneItemSnapshot,
        layer: VideoLayer,
        fps: u32,
        mixer: Option<&media_pp::elements::MixerHandle>,
        into: Target,
    ) -> Result<OpenOutcome, BackendError> {
        let composition = match into {
            Target::Canvas => None,
            Target::Scene(scene) => match self
                .scenes
                .open
                .current(&source::scene::key(scene), |composition| {
                    composition.extra.clone()
                }) {
                Some(composition) => Some(composition),
                None => {
                    return Ok(OpenOutcome::Absent(
                        "the Scene this belongs to is not being composited".to_owned(),
                    ));
                }
            },
        };
        let compositor = composition.as_ref().unwrap_or(&self.compositor);
        let opened = self.open_kind(item, layer, fps, mixer, compositor);
        if opened.is_err() {
            self.remove_source(&crate::engine::source::input_name(item));
        }
        opened
    }

    fn open_kind(
        &self,
        item: &SceneItemSnapshot,
        layer: VideoLayer,
        fps: u32,
        mixer: Option<&media_pp::elements::MixerHandle>,
        compositor: &Compositor,
    ) -> Result<OpenOutcome, BackendError> {
        match item.kind {
            SourceKind::DisplayCapture => source::display_capture::open(
                &self.screens,
                &self.gpu,
                compositor,
                item,
                layer,
                fps,
            ),
            SourceKind::WindowCapture => {
                source::window_capture::open(&self.screens, &self.gpu, compositor, item, layer, fps)
            }
            SourceKind::VideoCapture => {
                source::video_capture::open(&self.gpu, compositor, &self.cameras, item, layer)
            }
            SourceKind::MediaFile => source::media_file::open(
                &self.gpu,
                compositor,
                mixer,
                &self.meter_wake,
                item,
                layer,
            ),
            SourceKind::Rtsp => {
                source::rtsp::open(&self.gpu, compositor, mixer, &self.meter_wake, item, layer)
            }
            SourceKind::Image => source::image::open(&self.gpu, compositor, item, layer),
            SourceKind::Color => {
                source::color::open(&self.gpu, compositor, item, layer).map(OpenOutcome::Open)
            }
            SourceKind::Drawing => {
                source::drawing::open(&self.gpu, compositor, item, layer).map(OpenOutcome::Open)
            }
            SourceKind::Text => {
                source::text::open(&self.gpu, compositor, item, layer).map(OpenOutcome::Open)
            }
            SourceKind::Scene => source::scene::open(
                &self.gpu,
                compositor,
                &self.scenes,
                item,
                layer,
                fps,
                self.size,
            ),
            SourceKind::Browser => {
                source::browser::open(&self.gpu, compositor, mixer, &self.meter_wake, item, layer)
            }
        }
    }
}

/// One SceneItem's share of whatever is producing its frames — the Linux
/// twin's, and for its reasons.
pub(in crate::engine) enum RunningSource {
    /// A pipeline this item alone owns, such as a Color Source's pusher.
    Owned(Arc<Pipeline>),
    /// One branch of something other items may also be drawing from: a
    /// display, a window, a camera, or a Scene composited for another.
    Shared {
        capture: Arc<dyn SharedCapture>,
        key: String,
        share: Share,
    },
}

impl RunningSource {
    pub(in crate::engine) fn stats(&self) -> Option<media_pp::stats::PipelineStats> {
        match self {
            Self::Owned(pipeline) => Some(pipeline.stats()),
            Self::Shared {
                capture,
                key,
                share,
            } => capture.stats(key, *share),
        }
    }

    pub(in crate::engine) fn pause(&self) {
        match self {
            Self::Owned(pipeline) => pipeline.pause(),
            Self::Shared {
                capture,
                key,
                share,
            } => capture.set_showing(key, *share, false),
        }
    }

    pub(in crate::engine) fn resume(&self) {
        match self {
            Self::Owned(pipeline) => pipeline.resume(),
            Self::Shared {
                capture,
                key,
                share,
            } => capture.set_showing(key, *share, true),
        }
    }

    pub(in crate::engine) fn ended(&self) -> bool {
        match self {
            Self::Owned(pipeline) => super::pipeline_ended(pipeline),
            Self::Shared {
                capture,
                key,
                share,
            } => capture.ended(key, *share),
        }
    }

    pub(in crate::engine) fn stop(&self) {
        match self {
            Self::Owned(pipeline) => pipeline.stop(),
            Self::Shared {
                capture,
                key,
                share,
            } => capture.detach(key, *share),
        }
    }
}
