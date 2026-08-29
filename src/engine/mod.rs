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
use time::OffsetDateTime;

use crate::domain::{SceneCanvas, SceneItemId, SourceSettings, Transform};
use crate::project::{ProjectCommand, ProjectDispatcher, SourceCommand};
use crate::snapshots::{SceneItemSnapshot, SourcesSnapshot};

use backend::{Backend, BackendError, OpenSource};

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
    /// Whether anyone is looking at the Preview — a minimised window is
    /// nobody, and the frame then has nowhere worth going.
    PreviewVisible(bool),
    /// Start writing the composited frames to a file. Carries no path: where
    /// a recording goes is settled here, not by whoever pressed the button.
    StartRecording,
    /// Finish the running recording, closing its file.
    StopRecording,
    /// What the *next* recording is written as. A running one is unaffected:
    /// an mp4's header is written before its first frame, so none of this can
    /// be renegotiated after it has started.
    RecordingSettings(Box<crate::settings::RecordingSettings>),
}

/// What the engine is started with, as opposed to what it is told afterwards
/// over [`EngineCommand`]. Grouped because they travel together and because
/// `run` had collected more parameters than anyone can read at a glance.
struct EngineSetup {
    size: [u32; 2],
    project: Option<ProjectDispatcher>,
    /// What the *first* recording is written as, loaded from disk before the
    /// engine existed — see `ObsApp::new`.
    recording: crate::settings::RecordingSettings,
}

/// The slots the engine writes and the UI reads, which travel together.
struct Published {
    frame: Arc<ArcSwapOption<CompositeFrame>>,
    active_fps: Arc<AtomicU32>,
    /// When the running recording started, or `None` when none is.
    ///
    /// The engine publishes the instant rather than an elapsed time so the
    /// clock in the status bar advances between engine ticks — and it is
    /// written only once a recording has actually started, so a start that
    /// failed leaves the UI showing what is true.
    recording_since: Arc<ArcSwapOption<Instant>>,
    /// Which encoders the backend can open — see `EngineManager::encoders`.
    encoders: Arc<ArcSwapOption<Vec<crate::settings::RecordingEncoder>>>,
    /// Why the last attempt to start a recording failed, if it did.
    ///
    /// A failed start is otherwise silent: nothing appears, no clock runs,
    /// and the button goes back to what it said. Somewhere has to keep the
    /// reason, and it is the engine that has it.
    recording_error: Arc<ArcSwapOption<String>>,
}

pub struct EngineManager {
    frame: Arc<ArcSwapOption<CompositeFrame>>,
    /// `f32` bits, so the UI can read the rate without a lock.
    active_fps: Arc<AtomicU32>,
    recording_since: Arc<ArcSwapOption<Instant>>,
    recording_error: Arc<ArcSwapOption<String>>,
    /// Which H.264 encoders the backend can open. Published once, after the
    /// backend has been built — probing needs its device, and the answer
    /// cannot change while the application runs.
    encoders: Arc<ArcSwapOption<Vec<crate::settings::RecordingEncoder>>>,
    commands: Sender<EngineCommand>,
    stop: Arc<AtomicBool>,
    worker: Option<JoinHandle<()>>,
}

impl EngineManager {
    pub fn spawn(
        render_state: RenderState,
        canvas: SceneCanvas,
        project: Option<ProjectDispatcher>,
        recording_settings: crate::settings::RecordingSettings,
        wake_ui: impl Fn() + Send + Sync + 'static,
    ) -> std::io::Result<Self> {
        let size = [canvas.width as u32, canvas.height as u32];
        let frame = Arc::new(ArcSwapOption::empty());
        let active_fps = Arc::new(AtomicU32::new(0));
        let recording_since = Arc::new(ArcSwapOption::empty());
        let stop = Arc::new(AtomicBool::new(false));
        let recording_error = Arc::new(ArcSwapOption::empty());
        let encoders = Arc::new(ArcSwapOption::empty());
        let (commands, command_rx) = mpsc::channel();

        let worker = thread::Builder::new().name("engine".to_owned()).spawn({
            let frame = Arc::clone(&frame);
            let active_fps = Arc::clone(&active_fps);
            let recording_since = Arc::clone(&recording_since);
            let recording_error = Arc::clone(&recording_error);
            let encoders = Arc::clone(&encoders);
            let stop = Arc::clone(&stop);
            move || {
                let published = Published {
                    frame,
                    active_fps,
                    recording_since,
                    recording_error,
                    encoders,
                };
                let setup = EngineSetup {
                    size,
                    project,
                    recording: recording_settings,
                };
                if let Err(error) = run(render_state, setup, published, command_rx, &stop, wake_ui)
                {
                    // The Preview keeps showing "no frame" rather than the
                    // application failing to start over a compositor.
                    eprintln!("engine stopped: {error}");
                }
            }
        })?;

        Ok(Self {
            frame,
            active_fps,
            recording_since,
            recording_error,
            encoders,
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

    /// Tells the engine whether anyone is looking at the Preview.
    ///
    /// A minimised window is nobody: the frame still has to be composited,
    /// since the rate reported is what a recording would be made at, but
    /// putting it into the texture egui samples is work for a texture nobody
    /// will sample. Coming back into view is what makes the newest frame
    /// reach it, so the Preview is current rather than as it was when the
    /// window went down.
    pub fn set_preview_visible(&self, visible: bool) {
        let _ = self.commands.send(EngineCommand::PreviewVisible(visible));
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

    /// Starts writing the composited frames to a file.
    ///
    /// Asks rather than tells: the engine builds the encoder and the file on
    /// its own thread, and either can fail. [`EngineManager::recording`] is
    /// what says whether it worked, and it stays `None` if it did not.
    pub fn start_recording(&self) {
        let _ = self.commands.send(EngineCommand::StartRecording);
    }

    pub fn stop_recording(&self) {
        let _ = self.commands.send(EngineCommand::StopRecording);
    }

    /// Hands the engine what the next recording should be written as.
    pub fn set_recording_settings(&self, settings: crate::settings::RecordingSettings) {
        let _ = self
            .commands
            .send(EngineCommand::RecordingSettings(Box::new(settings)));
    }

    /// How long the running recording has been going, or `None` when none is.
    ///
    /// Derived from the instant the engine published rather than counted
    /// here, so the two cannot disagree about whether a recording exists.
    pub fn recording(&self) -> Option<Duration> {
        self.recording_since
            .load_full()
            .map(|since| since.elapsed())
    }

    /// Why the last attempt to start a recording failed, if it did.
    ///
    /// Cleared when the next attempt is made, so this always describes the
    /// most recent one rather than accumulating.
    /// Which H.264 encoders this machine can record with, or `None` before
    /// the engine has finished probing.
    pub fn encoders(&self) -> Option<Arc<Vec<crate::settings::RecordingEncoder>>> {
        self.encoders.load_full()
    }

    pub fn recording_error(&self) -> Option<Arc<String>> {
        self.recording_error.load_full()
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
    setup: EngineSetup,
    published: Published,
    commands: mpsc::Receiver<EngineCommand>,
    stop: &AtomicBool,
    wake_ui: impl Fn() + Send + Sync + 'static,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let EngineSetup {
        size,
        project,
        recording: mut recording_settings,
    } = setup;
    // Shared rather than moved: both the sink that publishes a frame and the
    // loop that puts the branch to sleep have to ask for a repaint.
    let wake_ui = Arc::new(wake_ui);
    let publish = {
        let frame = Arc::clone(&published.frame);
        let active_fps = Arc::clone(&published.active_fps);
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

    // Probed here rather than on demand: it needs the backend's own device,
    // and the dialog that shows the list must not be the thing that waits for
    // an encoder to open.
    published
        .encoders
        .store(Some(Arc::new(backend.available_encoders().to_vec())));

    let mut open = HashMap::new();
    let mut scene = SourcesSnapshot::default();
    // `recording_settings` is owned by this loop rather than shared: only
    // `StartRecording` reads it, and that arrives on the same channel a change
    // does, so one can never land half-way through a recording being opened.
    while !stop.load(Ordering::Acquire) {
        match commands.recv_timeout(Duration::from_millis(100)) {
            Ok(command) => {
                let mut reconciled = apply_command(
                    &backend,
                    project.as_ref(),
                    &mut open,
                    &mut scene,
                    &published,
                    &mut recording_settings,
                    command,
                );
                // Whatever else is already waiting, so a gesture's newer
                // positions are not left a poll behind the pointer.
                while let Ok(next) = commands.try_recv() {
                    reconciled |= apply_command(
                        &backend,
                        project.as_ref(),
                        &mut open,
                        &mut scene,
                        &published,
                        &mut recording_settings,
                        next,
                    );
                }
                if !reconciled {
                    continue;
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
    published: &Published,
    recording_settings: &mut crate::settings::RecordingSettings,
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
        EngineCommand::PreviewVisible(visible) => {
            backend.set_preview_visible(visible);
            false
        }
        EngineCommand::RecordingSettings(settings) => {
            *recording_settings = *settings;
            false
        }
        EngineCommand::StartRecording => {
            // Cleared before the attempt, not after: what is shown then
            // describes this attempt rather than an older one, and a retry
            // that works leaves nothing behind.
            published.recording_error.store(None);
            // The instant is published only on success, so a UI that shows a
            // recording running is showing one that is.
            match start_recording(backend, recording_settings) {
                Ok(started) => published.recording_since.store(Some(Arc::new(started))),
                Err(error) => {
                    let reason = describe(error.as_ref());
                    eprintln!("could not start recording: {reason}");
                    published.recording_error.store(Some(Arc::new(reason)));
                }
            }
            false
        }
        EngineCommand::StopRecording => {
            // Cleared whatever the backend says: a stop that failed has still
            // ended this recording as far as anything here can act on it, and
            // leaving the clock running would say otherwise.
            published.recording_since.store(None);
            if let Err(error) = backend.stop_recording() {
                eprintln!("could not stop recording cleanly: {error}");
            }
            false
        }
    }
}

/// Opens one recording, returning when it started rather than `()` — the
/// clock the status bar counts from is the moment the file began taking
/// frames, not the moment the button was pressed.
fn start_recording(
    backend: &Backend,
    settings: &crate::settings::RecordingSettings,
) -> Result<Instant, BackendError> {
    let path = crate::paths::recording_file_in(
        &settings.directory_or_default(),
        settings.prefix_or_default(),
        // A recording is named for the user's own wall clock. `now_local`
        // refuses to answer in a process with more than one thread on some
        // platforms, which this is; UTC is then a worse name rather than no
        // recording.
        OffsetDateTime::now_local().unwrap_or_else(|_| OffsetDateTime::now_utc()),
    );
    backend.start_recording(&path, TARGET_FPS, settings)?;
    println!("recording to {}", path.display());
    Ok(Instant::now())
}

/// One line naming everything that went wrong, not only the outermost of it.
///
/// `media-pp`'s errors carry their cause as a `source`, and the outer message
/// is often the general shape — "could not open the encoder" — while the one
/// a person can act on is underneath: no NVENC on this adapter, a directory
/// that cannot be written. So the chain is walked and joined.
///
/// A cause already quoted by its parent is not repeated: `thiserror`'s
/// `#[error("... {0}")]` embeds one, and appending it again would say the
/// same thing twice in the one line a status bar has.
fn describe(error: &(dyn std::error::Error + 'static)) -> String {
    let mut text = error.to_string();
    let mut next = error.source();
    while let Some(cause) = next {
        let message = cause.to_string();
        if !text.contains(&message) {
            text.push_str(": ");
            text.push_str(&message);
        }
        next = cause.source();
    }
    text
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

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug)]
    struct Layer {
        message: &'static str,
        cause: Option<Box<Layer>>,
    }

    impl std::fmt::Display for Layer {
        fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            formatter.write_str(self.message)
        }
    }

    impl std::error::Error for Layer {
        fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
            self.cause
                .as_deref()
                .map(|cause| cause as &(dyn std::error::Error + 'static))
        }
    }

    fn chain(messages: &[&'static str]) -> Layer {
        let mut layers = messages.iter().rev();
        let mut error = Layer {
            message: layers.next().expect("a chain needs a layer"),
            cause: None,
        };
        for message in layers {
            error = Layer {
                message,
                cause: Some(Box::new(error)),
            };
        }
        error
    }

    /// The outermost message is the shape of the failure; the one a person
    /// can act on is usually underneath it.
    #[test]
    fn a_failure_is_described_by_its_whole_chain() {
        let error = chain(&[
            "could not open the encoder",
            "avcodec_open2 failed",
            "no NVENC capable devices found",
        ]);

        assert_eq!(
            describe(&error),
            "could not open the encoder: avcodec_open2 failed: no NVENC capable devices found"
        );
    }

    /// `thiserror`'s `#[error("... {0}")]` already embeds its source, and a
    /// status bar has one line — saying it twice would spend half of that
    /// line repeating itself.
    #[test]
    fn a_cause_its_parent_already_quotes_is_not_repeated() {
        let error = chain(&[
            "opening the file failed: access is denied",
            "access is denied",
        ]);

        assert_eq!(
            describe(&error),
            "opening the file failed: access is denied"
        );
    }
}
