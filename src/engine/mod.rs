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

mod nv12;
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
    elements::{
        AppSink, CudaDevice, CudaDownload, CudaFrameFormat, CudaVideoCompositor,
        CudaVideoCompositorHandle, VideoCompositorOptions,
    },
    ffmpeg,
    pipeline::Pipeline,
    queue::OverflowPolicy,
};

use crate::domain::{SceneCanvas, SceneItemId, SourceKind};
use crate::snapshots::SourcesSnapshot;

use nv12::Nv12Target;
use source::{OpenSource, layer_for, open_display_capture};

/// The compositor's output rate. Independent of the egui repaint rate: egui
/// redraws when something asks it to, this advances on the compositor's own
/// clock.
const TARGET_FPS: u32 = 60;

/// What is behind every layer, and all there is to see before any Source is
/// connected. Deliberately not black: it has to be distinguishable from the
/// empty-Viewport fill the Preview paints when there is no frame at all.
const BACKGROUND: Color = Color::new(16, 40, 56);

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
    wake_ui: impl Fn() + Send + Sync + 'static,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    // Shared rather than moved: both the sink that publishes a frame and the
    // loop that puts the branch to sleep have to ask for a repaint.
    let wake_ui = Arc::new(wake_ui);
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
            frame_rate: ffmpeg::Rational::new(TARGET_FPS as i32, 1),
            background: BACKGROUND,
        },
    )?;

    // The compositor works in CUDA surfaces; this is the one place the frame
    // returns to system memory. Replacing this download with an import into
    // the planes above is what removes the round trip entirely.
    let download = CudaDownload::new(
        "preview-download",
        &device,
        CudaFrameFormat::Nv12,
        width,
        height,
    );

    // The sink runs on the compositor's own source thread, so neither the UI
    // thread nor this one does the conversion.
    let wgpu_device = render_state.device.clone();
    let queue = render_state.queue.clone();
    let published = Arc::clone(&frame);
    let reported_fps = Arc::clone(&active_fps);
    let wake_sink = Arc::clone(&wake_ui);
    let mut rate = FrameRate::new();
    let sink = AppSink::new("preview-out", move |buffer| {
        let MediaBuffer::Video(video) = buffer else {
            return Ok(());
        };
        if !target.draw(&wgpu_device, &queue, &video) {
            return Ok(());
        }
        frame.store(Some(Arc::new(CompositeFrame { texture_id })));
        if let Some(measured) = rate.tick() {
            active_fps.store(measured.to_bits(), Ordering::Relaxed);
        }
        wake_sink();
        Ok(())
    });

    let pipeline = Pipeline::new("preview", compositor, |source, context| {
        // The Preview must not set the compositor's pace. `CudaDownload`
        // waits for the GPU to finish before the CPU can read, and a
        // synchronous chain makes the compositor wait with it — which is what
        // dragged a 60 fps compositor down to exactly half that. Behind a
        // queue the download runs on its own thread, and a Preview that
        // cannot keep up drops frames instead of slowing the output everything
        // else will be built from.
        let branch = context
            .branch()
            .queue_with_policy("preview-queue", 1, OverflowPolicy::DropNewest)
            .pipe(download)
            .to(Box::new(sink))?;
        context.attach(source, 0, branch)?;
        Ok(())
    })?;
    pipeline.run()?;
    // Nothing has been composited yet, and an empty Scene never will be, so
    // the Preview branch starts asleep and is woken by the first Source.
    pipeline.pause();
    let mut compositing = false;

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
                reconcile(&device, &handle, &mut open, &latest);

                // A Scene with no running Source composites the background
                // colour, forever, at the full frame rate — a download and two
                // uploads per frame to redraw a picture that never changes.
                // Switching to such a Scene should cost nothing.
                let wanted = open
                    .values()
                    .any(|state| matches!(state, SourceState::Open(_)));
                if wanted != compositing {
                    if wanted {
                        pipeline.resume();
                    } else {
                        pipeline.pause();
                        // The texture still holds the Scene that was showing a
                        // moment ago, and leaving it up would attribute another
                        // Scene's picture to this one.
                        published.store(None);
                        reported_fps.store(0, Ordering::Relaxed);
                        wake_ui();
                    }
                    compositing = wanted;
                }
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
    device: &CudaDevice,
    handle: &CudaVideoCompositorHandle,
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
                let state = match open_display_capture(device, handle, item, layer, TARGET_FPS) {
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
