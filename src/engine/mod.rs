//! The compositing engine: everything that produces Preview pixels.
//!
//! It runs off the UI thread for the same reason `ProjectManager` does — the
//! work does not belong there — but the pressure here is different. A
//! 1920x1080 frame is 8 MB, so at 60 fps this moves roughly half a gigabyte
//! per second into GPU memory. Doing that inside `eframe::App::ui` would pay
//! for every frame twice: once to build it, once in the dropped input latency.
//!
//! What crosses the thread boundary is therefore not pixels but a
//! [`CompositeFrame`] — an already-uploaded texture the UI only has to name.
//! The upload uses a `wgpu::Queue` clone; `Queue` and `Device` are
//! `Send + Sync` and internally reference-counted, so the engine shares
//! eframe's device rather than opening a second one.

mod source;

use std::collections::HashMap;
use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicU32, Ordering},
    mpsc::{self, RecvTimeoutError, Sender},
};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use arc_swap::ArcSwapOption;
use eframe::egui;
use eframe::egui_wgpu::RenderState;
use media_pp::{
    buffer::MediaBuffer,
    color::Color,
    elements::{AppSink, SwVideoCompositor, SwVideoCompositorHandle, VideoCompositorOptions},
    ffmpeg,
    pipeline::Pipeline,
};

use crate::domain::{SceneCanvas, SceneItemId, SourceKind};
use crate::snapshots::SourcesSnapshot;

use source::{OpenSource, layer_for, open_display_capture};

/// The compositor's output rate. Independent of the egui repaint rate: egui
/// redraws when something asks it to, this advances on the compositor's own
/// clock.
const TARGET_FPS: u32 = 60;

/// What is behind every layer, and all there is to see before any Source is
/// connected. Deliberately not black: it has to be distinguishable from the
/// empty-Viewport fill the Preview paints when there is no frame at all.
const BACKGROUND: Color = Color::new(16, 40, 56);

/// `SwVideoCompositor` emits BGRA, and this is that byte order named for wgpu,
/// so frames upload with no repacking or channel swap.
///
/// egui's `register_native_texture` documents `Rgba8Unorm`, but it only builds
/// a bind group from the view, and both formats are
/// `TextureSampleType::Float { filterable: true }` — layout-compatible by
/// construction. The sampler returns correct RGBA either way, because the
/// format is what describes the memory order to the hardware.
const FRAME_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Bgra8Unorm;

/// One composited frame, already resident on the GPU.
///
/// The `TextureId` stays valid for the life of the engine: the texture is
/// created and registered once, and each frame overwrites its contents.
/// Registering per frame would take the egui renderer's write lock every
/// frame and stall the very thread this exists to keep free.
pub struct CompositeFrame {
    pub texture_id: egui::TextureId,
}

pub struct EngineManager {
    frame: Arc<ArcSwapOption<CompositeFrame>>,
    /// `f32` bits, so the UI can read the rate without a lock.
    active_fps: Arc<AtomicU32>,
    scenes: Sender<SourcesSnapshot>,
    stop: Arc<AtomicBool>,
    worker: Option<JoinHandle<()>>,
}

impl EngineManager {
    pub fn spawn(
        render_state: RenderState,
        canvas: SceneCanvas,
        wake_ui: impl Fn() + Send + Sync + 'static,
    ) -> std::io::Result<Self> {
        let size = [canvas.width as u32, canvas.height as u32];
        let frame = Arc::new(ArcSwapOption::empty());
        let active_fps = Arc::new(AtomicU32::new(0));
        let stop = Arc::new(AtomicBool::new(false));
        let (scenes, scene_rx) = mpsc::channel();

        let worker = thread::Builder::new().name("engine".to_owned()).spawn({
            let frame = Arc::clone(&frame);
            let active_fps = Arc::clone(&active_fps);
            let stop = Arc::clone(&stop);
            move || {
                if let Err(error) = run(
                    render_state,
                    size,
                    frame,
                    active_fps,
                    scene_rx,
                    &stop,
                    wake_ui,
                ) {
                    // The Preview keeps showing "no frame" rather than the
                    // application failing to start over a compositor.
                    eprintln!("engine stopped: {error}");
                }
            }
        })?;

        Ok(Self {
            frame,
            active_fps,
            scenes,
            stop,
            worker: Some(worker),
        })
    }

    /// Tells the engine what the selected Scene now contains.
    ///
    /// Sources are reconciled against the project snapshot rather than driven
    /// by the actions that changed it: a restart replays no actions but must
    /// still open everything the project holds, and selecting a Scene replaces
    /// the whole set at once.
    pub fn apply(&self, sources: &SourcesSnapshot) {
        let _ = self.scenes.send(sources.clone());
    }

    /// The most recent composited frame, or `None` before the first one.
    pub fn frame(&self) -> Option<Arc<CompositeFrame>> {
        self.frame.load_full()
    }

    /// Measured output rate, or `None` until a full window has been observed.
    pub fn active_fps(&self) -> Option<f32> {
        let bits = self.active_fps.load(Ordering::Relaxed);
        (bits != 0).then(|| f32::from_bits(bits))
    }

    pub fn target_fps(&self) -> f32 {
        TARGET_FPS as f32
    }
}

impl Drop for EngineManager {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(worker) = self.worker.take() {
            worker.thread().unpark();
            let _ = worker.join();
        }
    }
}

fn run(
    render_state: RenderState,
    size: [u32; 2],
    frame: Arc<ArcSwapOption<CompositeFrame>>,
    active_fps: Arc<AtomicU32>,
    scenes: mpsc::Receiver<SourcesSnapshot>,
    stop: &AtomicBool,
    wake_ui: impl Fn() + Send + 'static,
) -> media_pp::Result<()> {
    media_pp::init()?;
    let [width, height] = size;

    let texture = render_state
        .device
        .create_texture(&wgpu::TextureDescriptor {
            label: Some("composite-frame"),
            size: wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: FRAME_FORMAT,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
    let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
    let texture_id = render_state.renderer.write().register_native_texture(
        &render_state.device,
        &view,
        wgpu::FilterMode::Linear,
    );

    let (compositor, handle) = SwVideoCompositor::new(
        "preview-compositor",
        VideoCompositorOptions {
            width,
            height,
            frame_rate: ffmpeg::Rational::new(TARGET_FPS as i32, 1),
            background: BACKGROUND,
        },
    )?;

    // The sink runs on the compositor's own source thread, so the upload never
    // touches either the UI thread or this one.
    let queue = render_state.queue.clone();
    let mut rate = FrameRate::new();
    let sink = AppSink::new("preview-out", move |buffer| {
        let MediaBuffer::Video(video) = buffer else {
            return Ok(());
        };
        upload(&queue, &texture, &video);
        frame.store(Some(Arc::new(CompositeFrame { texture_id })));
        if let Some(measured) = rate.tick() {
            active_fps.store(measured.to_bits(), Ordering::Relaxed);
        }
        wake_ui();
        Ok(())
    });

    let pipeline = Pipeline::new("preview", compositor, |source, context| {
        let branch = context.branch().to(Box::new(sink))?;
        context.attach(source, 0, branch)?;
        Ok(())
    })?;
    pipeline.run()?;

    let mut open = HashMap::new();
    while !stop.load(Ordering::Acquire) {
        match scenes.recv_timeout(Duration::from_millis(100)) {
            Ok(snapshot) => {
                // Only the newest snapshot describes the project now; the ones
                // behind it were already superseded before this woke up.
                let mut latest = snapshot;
                while let Ok(newer) = scenes.try_recv() {
                    latest = newer;
                }
                reconcile(&handle, &mut open, &latest);
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => break,
        }
    }

    for (_, state) in open.drain() {
        if let SourceState::Open(source) = state {
            source.pipeline.stop();
        }
    }
    pipeline.stop();
    Ok(())
}

enum SourceState {
    Open(OpenSource),
    /// Opening failed once and will not be retried.
    ///
    /// A retry loop here would reopen the portal dialog on every snapshot,
    /// which is a stream of modal windows rather than an error message.
    Failed,
}

/// Brings the running Sources in line with what the project now holds.
fn reconcile(
    handle: &SwVideoCompositorHandle,
    open: &mut HashMap<SceneItemId, SourceState>,
    snapshot: &SourcesSnapshot,
) {
    let count = snapshot.items.len();
    for (index, item) in snapshot.items.iter().enumerate() {
        if item.kind != SourceKind::DisplayCapture {
            continue;
        }
        // The snapshot is ordered front-most first, and the compositor draws
        // larger z later, so the two run opposite ways.
        let layer = layer_for(item, (count - index) as i32);
        match open.get(&item.id) {
            Some(SourceState::Open(source)) => {
                let _ = source.layer.set_layer(layer);
            }
            Some(SourceState::Failed) => {}
            None => {
                let state = match open_display_capture(handle, item, layer, TARGET_FPS) {
                    Ok(source) => SourceState::Open(source),
                    Err(error) => {
                        eprintln!("could not open \"{}\": {error}", item.name);
                        SourceState::Failed
                    }
                };
                open.insert(item.id, state);
            }
        }
    }

    open.retain(|id, state| {
        if snapshot.items.iter().any(|item| item.id == *id) {
            return true;
        }
        if let SourceState::Open(source) = state {
            source.pipeline.stop();
            handle.remove_source(&source.name);
        }
        false
    });
}

/// Copies one composited frame into the texture the UI samples.
///
/// FFmpeg pads rows to its own alignment, so the frame's stride is passed
/// through rather than assumed: `write_texture` re-strides into its staging
/// buffer, which is why no repacking is needed here.
fn upload(queue: &wgpu::Queue, texture: &wgpu::Texture, video: &ffmpeg::frame::Video) {
    let size = texture.size();
    if video.width() != size.width || video.height() != size.height {
        // The compositor's output size is fixed at construction, so this can
        // only mean the two disagree about the Canvas — dropping the frame is
        // better than uploading a misaligned one.
        return;
    }
    queue.write_texture(
        texture.as_image_copy(),
        video.data(0),
        wgpu::TexelCopyBufferLayout {
            offset: 0,
            bytes_per_row: Some(video.stride(0) as u32),
            rows_per_image: Some(size.height),
        },
        size,
    );
}

/// Counts composited frames over a rolling one-second window.
struct FrameRate {
    window_start: Instant,
    frames: u32,
}

impl FrameRate {
    fn new() -> Self {
        Self {
            window_start: Instant::now(),
            frames: 0,
        }
    }

    fn tick(&mut self) -> Option<f32> {
        self.frames += 1;
        let elapsed = self.window_start.elapsed();
        if elapsed < Duration::from_secs(1) {
            return None;
        }
        let measured = self.frames as f32 / elapsed.as_secs_f32();
        self.window_start = Instant::now();
        self.frames = 0;
        Some(measured)
    }
}
