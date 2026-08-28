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

mod backend;

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
use media_pp::elements::{VideoFit, VideoLayer, VideoRect};

use crate::domain::{SceneCanvas, SceneItemId, SourceSettings, Transform};
use crate::project::{ProjectCommand, ProjectDispatcher, SourceCommand};
use crate::snapshots::{SceneItemSnapshot, SourcesSnapshot};

use backend::{Backend, OpenSource};

/// The compositor's output rate. Independent of the egui repaint rate: egui
/// redraws when something asks it to, this advances on the compositor's own
/// clock. It is what a recording will be made of, so it is not the knob to
/// turn for a quieter Preview.
const TARGET_FPS: u32 = 60;

/// How often the Preview is redrawn from those frames.
///
/// A Preview is not an output: it is a few hundred pixels wide and watched by
/// one person. Halving its rate took this application from 10% of a
/// twelve-core machine to 2.5%, and almost none of that is pixels —
/// downloading and resolving at 720p instead of 1080p was measured and
/// changed nothing. The cost is per-frame overhead, most of it the whole-UI
/// repaint that each drawn frame asks egui for.
const PREVIEW_FPS: u32 = 30;

/// One composited frame, already resident on the GPU.
///
/// The `TextureId` stays valid for the life of the engine: the texture is
/// created and registered once, and each frame overwrites its contents.
/// Registering per frame would take the egui renderer's write lock every
/// frame and stall the very thread this exists to keep free.
pub struct CompositeFrame {
    pub texture_id: egui::TextureId,
}

/// What the application asks the engine to change.
enum EngineCommand {
    /// The selected Scene's contents, as the project now holds them.
    Scene(Box<SourcesSnapshot>),
    /// One item's Transform mid-gesture, which the project does not hold yet.
    Dragging(SceneItemId, Transform),
}

/// The two slots the engine writes and the UI reads, which travel together.
struct Published {
    frame: Arc<ArcSwapOption<CompositeFrame>>,
    active_fps: Arc<AtomicU32>,
}

pub struct EngineManager {
    frame: Arc<ArcSwapOption<CompositeFrame>>,
    /// `f32` bits, so the UI can read the rate without a lock.
    active_fps: Arc<AtomicU32>,
    commands: Sender<EngineCommand>,
    stop: Arc<AtomicBool>,
    worker: Option<JoinHandle<()>>,
}

impl EngineManager {
    pub fn spawn(
        render_state: RenderState,
        canvas: SceneCanvas,
        project: Option<ProjectDispatcher>,
        wake_ui: impl Fn() + Send + Sync + 'static,
    ) -> std::io::Result<Self> {
        let size = [canvas.width as u32, canvas.height as u32];
        let frame = Arc::new(ArcSwapOption::empty());
        let active_fps = Arc::new(AtomicU32::new(0));
        let stop = Arc::new(AtomicBool::new(false));
        let (commands, command_rx) = mpsc::channel();

        let worker = thread::Builder::new().name("engine".to_owned()).spawn({
            let frame = Arc::clone(&frame);
            let active_fps = Arc::clone(&active_fps);
            let stop = Arc::clone(&stop);
            move || {
                let published = Published { frame, active_fps };
                if let Err(error) = run(
                    render_state,
                    size,
                    project,
                    published,
                    command_rx,
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
            commands,
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
        let _ = self
            .commands
            .send(EngineCommand::Scene(Box::new(sources.clone())));
    }

    /// Moves one layer while the pointer is still down.
    ///
    /// The project database only learns the Transform when the gesture ends,
    /// which is correct — a drag is not a series of edits. But the compositor
    /// would then show the item where it used to be until the pointer is
    /// released, with the gizmo somewhere else entirely, so the layer follows
    /// the gesture directly and the snapshot confirms it afterwards.
    pub fn set_dragging_transform(&self, item: SceneItemId, transform: Transform) {
        let _ = self.commands.send(EngineCommand::Dragging(item, transform));
    }

    /// The most recent composited frame, or `None` before the first one.
    pub fn frame(&self) -> Option<Arc<CompositeFrame>> {
        self.frame.load_full()
    }

    /// The compositor's measured output rate, or `None` until a full window
    /// has been observed.
    ///
    /// Counts every composited frame, not the ones the Preview drew: this is
    /// the rate an output would be recorded at, and the Preview deliberately
    /// redraws less often than it.
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
    project: Option<ProjectDispatcher>,
    published: Published,
    commands: mpsc::Receiver<EngineCommand>,
    stop: &AtomicBool,
    wake_ui: impl Fn() + Send + Sync + 'static,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    // Shared rather than moved: both the sink that publishes a frame and the
    // loop that puts the branch to sleep have to ask for a repaint.
    let wake_ui = Arc::new(wake_ui);
    let Published { frame, active_fps } = published;
    let publish = {
        let frame = Arc::clone(&frame);
        let active_fps = Arc::clone(&active_fps);
        let wake_ui = Arc::clone(&wake_ui);
        let rate = std::sync::Mutex::new(FrameRate::new());
        move |texture_id| {
            if let Some(texture_id) = texture_id {
                frame.store(Some(Arc::new(CompositeFrame { texture_id })));
            }
            if let Some(measured) = rate.lock().expect("never poisoned").tick() {
                active_fps.store(measured.to_bits(), Ordering::Relaxed);
            }
            if texture_id.is_some() {
                wake_ui();
            }
        }
    };
    let backend = Backend::start(&render_state, size, TARGET_FPS, PREVIEW_FPS, publish)?;

    // Nothing has been composited yet, and an empty Scene never will be, so
    // the Preview branch starts asleep and is woken by the first Source.
    backend.pause();
    let mut compositing = false;

    let mut open = HashMap::new();
    let mut scene = SourcesSnapshot::default();
    while !stop.load(Ordering::Acquire) {
        match commands.recv_timeout(Duration::from_millis(100)) {
            Ok(command) => {
                let mut reconciled =
                    apply_command(&backend, project.as_ref(), &mut open, &mut scene, command);
                // Whatever else is already waiting, so a gesture's newer
                // positions are not left a poll behind the pointer.
                while let Ok(next) = commands.try_recv() {
                    reconciled |=
                        apply_command(&backend, project.as_ref(), &mut open, &mut scene, next);
                }
                if !reconciled {
                    continue;
                }

                // A Scene with no running Source composites the background
                // colour, forever, at the full frame rate — a download and two
                // uploads per frame to redraw a picture that never changes.
                // Switching to such a Scene should cost nothing.
                let wanted = open
                    .values()
                    .any(|state| matches!(state, SourceState::Open(source) if source.showing));
                if wanted != compositing {
                    if wanted {
                        backend.resume();
                    } else {
                        backend.pause();
                        // The texture still holds the Scene that was showing a
                        // moment ago, and leaving it up would attribute another
                        // Scene's picture to this one.
                        frame.store(None);
                        active_fps.store(0, Ordering::Relaxed);
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
            source.source.stop();
        }
    }
    backend.stop();
    Ok(())
}

/// Applies one change, reporting whether the running Sources may have moved
/// on — a Scene change can start or stop them, a drag never does.
fn apply_command(
    backend: &Backend,
    project: Option<&ProjectDispatcher>,
    open: &mut HashMap<SceneItemId, SourceState>,
    scene: &mut SourcesSnapshot,
    command: EngineCommand,
) -> bool {
    match command {
        EngineCommand::Scene(snapshot) => {
            *scene = *snapshot;
            reconcile(backend, project, open, scene);
            true
        }
        EngineCommand::Dragging(item_id, transform) => {
            let Some(index) = scene.items.iter().position(|item| item.id == item_id) else {
                return false;
            };
            let Some(SourceState::Open(source)) = open.get(&item_id) else {
                return false;
            };
            let item = &scene.items[index];
            let layer = layer_for(item, transform, (scene.items.len() - index) as i32);
            let _ = source.layer.set_layer(layer);
            false
        }
    }
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
    backend: &Backend,
    project: Option<&ProjectDispatcher>,
    open: &mut HashMap<SceneItemId, SourceState>,
    snapshot: &SourcesSnapshot,
) {
    let count = snapshot.items.len();
    for (index, item) in snapshot.items.iter().enumerate() {
        // The snapshot is ordered front-most first, and the compositor draws
        // larger z later, so the two run opposite ways.
        let layer = layer_for(item, item.transform, (count - index) as i32);
        match open.get(&item.id) {
            Some(SourceState::Open(source)) => {
                let _ = source.layer.set_layer(layer);
            }
            Some(SourceState::Failed) => {}
            None => {
                let state = match backend.open_source(item, layer, TARGET_FPS) {
                    Ok(source) => {
                        // The portal may hand back a different token than the
                        // one it was given. Keeping the old one would mean
                        // prompting on every launch, which is the thing
                        // persisting it was for.
                        if let (Some(project), Some(token)) =
                            (project, source.refreshed_token.clone())
                        {
                            project.dispatch(ProjectCommand::Source(
                                SourceCommand::SetRestoreToken(item.id, token),
                            ));
                        }
                        SourceState::Open(source)
                    }
                    Err(error) => {
                        eprintln!("could not open \"{}\": {error}", item.name);
                        SourceState::Failed
                    }
                };
                open.insert(item.id, state);
            }
        }
    }

    // A Source whose item merely left the Scene is kept, stopped: coming back
    // to that Scene is then a resume rather than another portal round trip,
    // and a stopped capture costs nothing while it waits.
    for (id, state) in open.iter_mut() {
        let SourceState::Open(source) = state else {
            continue;
        };
        let showing = snapshot.items.iter().any(|item| item.id == *id);
        if showing == source.showing {
            continue;
        }
        if showing {
            source.source.resume();
        } else {
            source.source.pause();
            let _ = source.layer.set_visible(false);
        }
        source.showing = showing;
    }

    // Only an item the project no longer holds anywhere is closed for good.
    open.retain(|id, state| {
        if snapshot.live_items.contains(id) {
            return true;
        }
        if let SourceState::Open(source) = state {
            source.source.stop();
            backend.remove_source(&source.name);
        }
        false
    });
}

/// Where a SceneItem's layer sits on the Canvas, and in what order.
///
/// The rectangle already carries the Source's own size scaled by the item's
/// Transform, so the fit is [`VideoFit::Stretch`]: whatever aspect the user
/// asked for is expressed in that rectangle, and letterboxing inside it would
/// second-guess them.
fn layer_for(item: &SceneItemSnapshot, transform: Transform, z_index: i32) -> VideoLayer {
    let [x, y, width, height] = item.canvas_rect(transform);
    let mut layer = VideoLayer::new(VideoRect::new(
        x.round() as i32,
        y.round() as i32,
        (width.round() as u32).max(1),
        (height.round() as u32).max(1),
    ));
    layer.z_index = z_index;
    layer.visible = item.visible;
    layer.fit = VideoFit::Stretch;
    // NV12 carries no alpha, so a Color Source's own is the layer's opacity
    // rather than something the blend could read out of its pixels.
    if let SourceSettings::Color(settings) = &item.settings {
        layer.opacity = f32::from(settings.rgba[3]) / 255.0;
    }
    layer
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
