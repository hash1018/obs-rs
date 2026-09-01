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

mod audio;
mod backend;
mod preview;
mod recording;

pub use preview::CompositeFrame;
mod source;

pub use audio::AudioManager;

use std::collections::HashMap;
use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicU32, Ordering},
    mpsc::{self, RecvTimeoutError, Sender},
};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use arc_swap::ArcSwapOption;
use eframe::egui_wgpu::RenderState;
use media_pp::elements::{VideoFit, VideoLayer, VideoRect};
use time::OffsetDateTime;

use crate::domain::{SceneCanvas, SceneItemId, SourceKind, SourceSettings, Transform};
use crate::project::{ProjectCommand, ProjectDispatcher, SourceCommand};
use crate::snapshots::{SceneItemSnapshot, SourceStatus, SourcesSnapshot};

use backend::{Backend, BackendError};
use source::{OpenSource, PushedContent, push_content, refresh_media_file, refresh_pushed};

/// The rate to assume when the compositor cannot be asked — it is gone, or
/// there is no backend at all.
///
/// Not what the compositor runs at: that is `RecordingSettings::fps`, which
/// it is started with and follows afterwards. This is only the answer to
/// "what rate would a recording be configured for" when there is nothing to
/// ask, where any number is wrong and a plausible one beats a panic.
pub(in crate::engine) const TARGET_FPS: u32 = crate::settings::DEFAULT_FPS;

/// The most often the Preview is redrawn from those frames.
///
/// A ceiling, not a rate: what it actually redraws at is this or the
/// compositor's own rate, whichever is lower, so a Scene composited at 24
/// gives a Preview at 24 rather than one asking for frames that are not
/// being made.
///
/// A Preview is not an output: it is a few hundred pixels wide and watched by
/// one person. Halving its rate took this application from 10% of a
/// twelve-core machine to 2.5%, and almost none of that is pixels —
/// downloading and resolving at 720p instead of 1080p was measured and
/// changed nothing. The cost is per-frame overhead, most of it the whole-UI
/// repaint that each drawn frame asks egui for. That measurement is why this
/// stays a ceiling rather than following the compositor all the way up:
/// composing at 60 is a reason to record at 60, not a reason to repaint the
/// whole window 60 times a second for one person watching a thumbnail.
const PREVIEW_FPS: u32 = 30;

/// What the application asks the engine to change.
enum EngineCommand {
    /// The selected Scene's contents, as the project now holds them.
    Scene(Box<SourcesSnapshot>),
    /// Open this item's Source again, at the user's request — see
    /// `EngineManager::reopen_source`.
    ReopenSource(SceneItemId),
    /// One item's Transform mid-gesture, which the project does not hold yet.
    Dragging(SceneItemId, Transform),
    /// A Drawing's strokes mid-gesture, for the same reason `Dragging` exists:
    /// the mark has to appear under the pointer, and the project is not told
    /// until the pointer comes up. Carries the whole list rather than the one
    /// new stroke, because rasterizing is done from the list either way.
    Drawing(SceneItemId, Vec<crate::domain::Stroke>),
    /// A Color Source.s colour while the picker is still held, for the same
    /// reason `Drawing` exists: the picture has to follow the pointer, and
    /// the project is told once when it is let go.
    Colour(SceneItemId, [u8; 4]),
    /// A media file Source's gain while the fader is still held — the audio
    /// counterpart of `Colour`, and on this thread rather than the audio one
    /// because a file's fader belongs to its own pipeline.
    MediaGain(SceneItemId, f32),
    /// Whether anyone is looking at the Preview — a minimised window is
    /// nobody, and the frame then has nowhere worth going.
    PreviewVisible(bool),
    /// Start writing the composited frames to a file. Carries no path: where
    /// a recording goes is settled here, not by whoever pressed the button.
    StartRecording,
    /// Finish the running recording, closing its file.
    StopRecording,
    /// Stop or resume writing frames to the running recording, leaving its
    /// file open and taking no time out of the compositor.
    PauseRecording(bool),
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
    recording: RecordingState,
}

/// Everything a recording is opened from, and the one that is open.
///
/// Grouped because they are only ever reached together, and because
/// `apply_command` had collected as many parameters as it can carry.
struct RecordingState {
    /// What the *next* recording is written as. Loaded from disk before the
    /// engine existed — see `ObsApp::new` — and replaced whenever the
    /// Settings dialog is applied.
    settings: crate::settings::RecordingSettings,
    /// The mixer, taken once at startup because it lives on a thread this one
    /// cannot ask. `None` when it never started, which records video only and
    /// plays media files without their sound.
    ///
    /// Two things attach to it from here: a recording's audio track, on the
    /// `Tee`, and a media file Source's own audio, as one more mixer input.
    /// It sits on this struct because a recording was the first of them; the
    /// second arriving is not on its own a reason to move it.
    mixer: Option<(
        media_pp::elements::TeeHandle,
        media_pp::elements::MixerHandle,
    )>,
    /// Which audio codecs the linked FFmpeg carries, probed once at startup
    /// beside the video list. Kept so a stored codec that cannot open falls
    /// back rather than failing the recording — see [`usable_settings`].
    audio_codecs: Vec<crate::settings::RecordingAudioCodec>,
    /// The recording that is running, if one is. It rather than the backend
    /// holds the video branch too — see [`recording::Recording`].
    running: Option<recording::Recording>,
}

impl RecordingState {
    /// The mixer's own control, for whatever attaches an input to it.
    fn mixer_handle(&self) -> Option<&media_pp::elements::MixerHandle> {
        self.mixer.as_ref().map(|(_, mixer)| mixer)
    }

    /// What the mixer is actually summing into, or the default when it never
    /// started.
    ///
    /// Asked of the mixer rather than of the settings, because a format it
    /// refused leaves the old one running and the audio encoder has to be
    /// opened for what is really arriving.
    fn mix_format(&self) -> media_pp::elements::MixFormat {
        self.mixer
            .as_ref()
            .and_then(|(_, handle)| handle.mix_format())
            .unwrap_or(audio::DEFAULT_MIX_FORMAT)
    }
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
    /// When the running recording was paused, if it is.
    ///
    /// An instant rather than a flag because the clock is read from it:
    /// while paused it is what the elapsed time is measured *to*, so the
    /// figure stops moving without the UI being told again on every pass.
    recording_paused_at: Arc<ArcSwapOption<Instant>>,
    /// The SceneItems whose Source is not running, and why: a window that has
    /// closed, one that never opened, or a file that played out. The Sources
    /// list says so beside them, which is the only thing that explains an
    /// item drawing nothing.
    source_status: Arc<ArcSwapOption<HashMap<SceneItemId, SourceStatus>>>,
    /// Which encoders the backend can open — see `EngineManager::encoders`.
    encoders: Arc<ArcSwapOption<Vec<crate::settings::RecordingEncoder>>>,
    /// Which audio codecs this FFmpeg build can open — see
    /// `EngineManager::audio_codecs`.
    audio_codecs: Arc<ArcSwapOption<Vec<crate::settings::RecordingAudioCodec>>>,
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
    /// When the running recording was paused, if it is.
    ///
    /// An instant rather than a flag because the clock is read from it:
    /// while paused it is what the elapsed time is measured *to*, so the
    /// figure stops moving without the UI being told again on every pass.
    recording_paused_at: Arc<ArcSwapOption<Instant>>,
    recording_error: Arc<ArcSwapOption<String>>,
    /// The SceneItems drawing nothing — see `Published::source_status`.
    source_status: Arc<ArcSwapOption<HashMap<SceneItemId, SourceStatus>>>,
    /// Which H.264 encoders the backend can open. Published once, after the
    /// backend has been built — probing needs its device, and the answer
    /// cannot change while the application runs.
    encoders: Arc<ArcSwapOption<Vec<crate::settings::RecordingEncoder>>>,
    /// Which audio codecs the linked FFmpeg carries. Published beside the
    /// video list, though this one needs no device — one answer arriving at
    /// a time the dialog can already be open is enough.
    audio_codecs: Arc<ArcSwapOption<Vec<crate::settings::RecordingAudioCodec>>>,
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
        // Where a recording's audio track attaches — see
        // `AudioManager::mixer_tee`. `None` records without sound.
        mixer: Option<(
            media_pp::elements::TeeHandle,
            media_pp::elements::MixerHandle,
        )>,
        wake_ui: impl Fn() + Send + Sync + 'static,
    ) -> std::io::Result<Self> {
        let size = [canvas.width as u32, canvas.height as u32];
        let frame = Arc::new(ArcSwapOption::empty());
        let active_fps = Arc::new(AtomicU32::new(0));
        let recording_since = Arc::new(ArcSwapOption::empty());
        let recording_paused_at = Arc::new(ArcSwapOption::empty());
        let stop = Arc::new(AtomicBool::new(false));
        let recording_error = Arc::new(ArcSwapOption::empty());
        let source_status = Arc::new(ArcSwapOption::empty());
        let encoders = Arc::new(ArcSwapOption::empty());
        let audio_codecs = Arc::new(ArcSwapOption::empty());
        let (commands, command_rx) = mpsc::channel();

        let worker = thread::Builder::new().name("engine".to_owned()).spawn({
            let frame = Arc::clone(&frame);
            let active_fps = Arc::clone(&active_fps);
            let recording_since = Arc::clone(&recording_since);
            let recording_paused_at = Arc::clone(&recording_paused_at);
            let recording_error = Arc::clone(&recording_error);
            let source_status = Arc::clone(&source_status);
            let encoders = Arc::clone(&encoders);
            let audio_codecs = Arc::clone(&audio_codecs);
            let stop = Arc::clone(&stop);
            move || {
                let published = Published {
                    frame,
                    active_fps,
                    recording_since,
                    recording_paused_at,
                    recording_error,
                    source_status,
                    encoders,
                    audio_codecs,
                };
                let setup = EngineSetup {
                    size,
                    project,
                    recording: RecordingState {
                        settings: recording_settings,
                        mixer,
                        // Filled in once the probe has run — see `run`.
                        audio_codecs: Vec::new(),
                        running: None,
                    },
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
            recording_paused_at,
            recording_error,
            source_status,
            encoders,
            audio_codecs,
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

    /// Redraws a Drawing while the pointer is still down.
    ///
    /// The same arrangement `set_dragging_transform` has, and for the same
    /// reason: the project learns a stroke when the gesture ends, but the mark
    /// has to be under the pointer before that or drawing is unusable. What is
    /// sent is the whole list — every committed stroke plus the one being
    /// made — because that is what a redraw is built from either way.
    pub fn set_drawing_strokes(&self, item: SceneItemId, strokes: Vec<crate::domain::Stroke>) {
        let _ = self.commands.send(EngineCommand::Drawing(item, strokes));
    }

    /// Repaints a Color Source while its picker is still held, for the same
    /// reason [`Self::set_drawing_strokes`] exists.
    pub fn set_source_colour(&self, item: SceneItemId, rgba: [u8; 4]) {
        let _ = self.commands.send(EngineCommand::Colour(item, rgba));
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

    /// The rate the compositor is being asked for, which is also the rate a
    /// recording is written at.
    ///
    /// The setting rather than the constant, because the compositor now
    /// follows it — see `EngineCommand::RecordingSettings`. It is what the
    /// status bar's actual rate is compared against, so a machine keeping 30
    /// of a requested 30 reads as keeping up rather than as half of 60.
    pub fn target_fps(&self, settings: &crate::settings::RecordingSettings) -> f32 {
        settings.fps.max(1) as f32
    }

    /// Starts writing the composited frames to a file.
    ///
    /// Asks rather than tells: the engine builds the encoder and the file on
    /// its own thread, and either can fail. [`EngineManager::recording`] is
    /// what says whether it worked, and it stays `None` if it did not.
    pub fn start_recording(&self) {
        let _ = self.commands.send(EngineCommand::StartRecording);
    }

    /// Stops or resumes the running recording.
    ///
    /// The clock the status bar shows stops with it: what it counts is how
    /// long the file is, and a paused recording is not getting any longer.
    pub fn set_recording_paused(&self, paused: bool) {
        let _ = self.commands.send(EngineCommand::PauseRecording(paused));
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
    /// How long the running recording's file is, which is not how long ago
    /// it was started: a paused span is not in the file and must not be in
    /// the figure either.
    pub fn recording(&self) -> Option<Duration> {
        let since = self.recording_since.load_full()?;
        Some(match self.recording_paused_at.load_full() {
            Some(paused_at) => paused_at.saturating_duration_since(*since),
            None => since.elapsed(),
        })
    }

    /// Whether the running recording is paused.
    pub fn recording_paused(&self) -> bool {
        self.recording_paused_at.load().is_some()
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

    /// Which audio codecs this build can record with, or `None` before the
    /// engine has finished probing.
    pub fn audio_codecs(&self) -> Option<Arc<Vec<crate::settings::RecordingAudioCodec>>> {
        self.audio_codecs.load_full()
    }

    pub fn recording_error(&self) -> Option<Arc<String>> {
        self.recording_error.load_full()
    }

    /// The SceneItems that are not producing a picture right now.
    pub fn source_status(&self) -> Option<Arc<HashMap<SceneItemId, SourceStatus>>> {
        self.source_status.load_full()
    }

    /// One media file Source's gain while the fader is still held, for the
    /// same reason `set_source_colour` exists: what is heard has to follow
    /// the pointer, and the project is told once when the gesture ends.
    pub fn set_media_gain_db(&self, item: SceneItemId, gain_db: f32) {
        let _ = self.commands.send(EngineCommand::MediaGain(item, gain_db));
    }

    /// Asks for one Source to be opened again, whatever that costs.
    ///
    /// This is the only way a `Disconnected` Source comes back, and it exists
    /// because on Linux opening a Window Capture puts the portal's picker on
    /// screen. Nothing may do that on its own — see `SourceState::Disconnected`
    /// — so it waits here for someone to ask.
    pub fn reopen_source(&self, item: SceneItemId) {
        let _ = self.commands.send(EngineCommand::ReopenSource(item));
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
        mut recording,
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
    // Built at the configured rate, not at a constant. Composing at 60 for a
    // recording written at 30 is half the GPU cost thrown away, and the
    // setting is what the compositor follows from here on — see
    // `EngineCommand::RecordingSettings`.
    let backend = Backend::start(
        &render_state,
        size,
        recording.settings.fps.max(1),
        // Never above what is being composited: a Preview asking for 30 of a
        // Scene made at 24 is asking for frames that do not exist.
        PREVIEW_FPS.min(recording.settings.fps.max(1)),
        publish,
    )?;

    // Probed here rather than on demand: it needs the backend's own device,
    // and the dialog that shows the list must not be the thing that waits for
    // an encoder to open.
    published
        .encoders
        .store(Some(Arc::new(backend.available_encoders().to_vec())));
    // No device needed for these, but published from the same place so the
    // dialog has one moment at which both lists exist. Kept as well as
    // published: `usable_settings` needs it and cannot read a published slot
    // the UI owns.
    recording.audio_codecs = recording::available_audio_codecs(recording.mix_format());
    published
        .audio_codecs
        .store(Some(Arc::new(recording.audio_codecs.clone())));

    let mut open = HashMap::new();
    let mut scene = SourcesSnapshot::default();
    let mut looked_for_missing = Instant::now();
    // `recording` is owned by this loop rather than shared: only the commands
    // below reach it, and a settings change arrives on the same channel a
    // start does, so one can never land half-way through a recording being
    // opened.
    while !stop.load(Ordering::Acquire) {
        match commands.recv_timeout(Duration::from_millis(100)) {
            Ok(command) => {
                let mut reconciled = apply_command(
                    &backend,
                    project.as_ref(),
                    &mut open,
                    &mut scene,
                    &published,
                    &mut recording,
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
                        &mut recording,
                        next,
                    );
                }
                if !reconciled {
                    continue;
                }
            }
            Err(RecvTimeoutError::Timeout) => {
                if looked_for_missing.elapsed() < MISSING_RETRY {
                    continue;
                }
                looked_for_missing = Instant::now();
                notice_closed_windows(&backend, &mut open, &scene);
                notice_ended_media(&backend, &mut open, &scene);
                retry_missing(
                    &backend,
                    project.as_ref(),
                    recording.mixer_handle(),
                    &mut open,
                    &scene,
                );
            }
            Err(RecvTimeoutError::Disconnected) => break,
        }
        // Only where something may have opened or closed, which is what the
        // `continue`s above skip past: this is a comparison against what the
        // UI already holds, not something to do sixty times a second for an
        // answer that has not moved.
        publish_source_status(&published, &open);
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
    recording: &mut RecordingState,
    command: EngineCommand,
) -> bool {
    match command {
        EngineCommand::Scene(snapshot) => {
            *scene = *snapshot;
            reconcile(backend, project, recording.mixer_handle(), open, scene);
            true
        }
        EngineCommand::ReopenSource(item_id) => {
            let Some(index) = scene.items.iter().position(|item| item.id == item_id) else {
                return false;
            };
            // Stopped first where something is still open: asking again for a
            // Source that is running would leave the old one behind, holding
            // its layer and its capture.
            if let Some(SourceState::Open(source)) = open.get(&item_id) {
                source.source.stop();
                backend.remove_source(&source.name);
            }
            let item = &scene.items[index];
            let layer = layer_for(item, item.transform, (scene.items.len() - index) as i32);
            let state = open_item(backend, project, recording.mixer_handle(), item, layer);
            open.insert(item_id, state);
            true
        }
        EngineCommand::Drawing(item_id, strokes) => {
            if let Some(SourceState::Open(source)) = open.get_mut(&item_id) {
                push_content(source, PushedContent::Drawing(strokes));
            }
            false
        }
        EngineCommand::Colour(item_id, rgba) => {
            if let Some(SourceState::Open(source)) = open.get_mut(&item_id) {
                push_content(source, PushedContent::Color(rgba));
            }
            false
        }
        EngineCommand::MediaGain(item_id, gain_db) => {
            if let Some(SourceState::Open(source)) = open.get(&item_id) {
                source::set_media_gain_db(source, gain_db);
            }
            false
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
            // The rate is the compositor's, not just the file's: what a
            // recording is written at is what is being composited, so
            // applying it means telling the compositor. Refused while one is
            // running — the encoder was configured for the old rate and the
            // timestamps it is being handed would change meaning underneath
            // it. The setting is kept either way, and takes at the next
            // change once the recording has stopped.
            if recording.running.is_none() && settings.fps != backend.frame_rate() {
                backend.set_frame_rate(settings.fps);
            }
            recording.settings = *settings;
            // The rate the mix runs at decides which audio encoders can open —
            // `libopus` takes 48 kHz and a short list of others, and nothing
            // else. Re-probed here because Apply is when it can have moved.
            recording.audio_codecs = recording::available_audio_codecs(recording.mix_format());
            published
                .audio_codecs
                .store(Some(Arc::new(recording.audio_codecs.clone())));
            false
        }
        EngineCommand::StartRecording => {
            // Cleared before the attempt, not after: what is shown then
            // describes this attempt rather than an older one, and a retry
            // that works leaves nothing behind.
            published.recording_error.store(None);
            // A previous run's pause must not carry into this one.
            published.recording_paused_at.store(None);
            // The instant is published only on success, so a UI that shows a
            // recording running is showing one that is.
            match start_recording(backend, recording) {
                Ok(started) => published.recording_since.store(Some(Arc::new(started))),
                Err(error) => {
                    let reason = describe(error.as_ref());
                    eprintln!("could not start recording: {reason}");
                    published.recording_error.store(Some(Arc::new(reason)));
                }
            }
            false
        }
        EngineCommand::PauseRecording(paused) => {
            let Some(running) = recording.running.as_ref() else {
                eprintln!("no recording is running");
                return false;
            };
            running.set_paused(paused);
            // The clock counts how long the file is. Pausing stops it where
            // it is; resuming moves the start forward by however long the
            // pause lasted, so the same subtraction keeps working without the
            // UI being told anything on every pass.
            match (paused, published.recording_paused_at.load_full()) {
                (true, None) => published
                    .recording_paused_at
                    .store(Some(Arc::new(Instant::now()))),
                (false, Some(paused_at)) => {
                    if let Some(since) = published.recording_since.load_full() {
                        let elapsed = paused_at.elapsed();
                        published
                            .recording_since
                            .store(Some(Arc::new(*since + elapsed)));
                    }
                    published.recording_paused_at.store(None);
                }
                _ => {}
            }
            false
        }
        EngineCommand::StopRecording => {
            // Cleared whatever the backend says: a stop that failed has still
            // ended this recording as far as anything here can act on it, and
            // leaving the clock running would say otherwise.
            published.recording_since.store(None);
            published.recording_paused_at.store(None);
            match recording.running.take() {
                Some(running) => {
                    if let Err(error) = running.stop(backend) {
                        eprintln!("could not stop recording cleanly: {error}");
                    }
                }
                None => eprintln!("no recording is running"),
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
    recording: &mut RecordingState,
) -> Result<Instant, BackendError> {
    if recording.running.is_some() {
        return Err("a recording is already running".into());
    }
    // Probed here rather than taken from the list published at startup: the
    // mix format can have moved since, and which encoders open depends on it.
    // Two `avcodec_open2` calls, beside a video encoder and a muxer that are
    // about to be opened anyway.
    let audio_codecs = recording::available_audio_codecs(recording.mix_format());
    let settings = usable_settings(backend, &audio_codecs, &recording.settings);
    let settings = &settings;
    let path = crate::paths::recording_file_in(
        &settings.directory_or_default(),
        settings.prefix_or_default(),
        // A recording is named for the user's own wall clock. `now_local`
        // refuses to answer in a process with more than one thread on some
        // platforms, which this is; UTC is then a worse name rather than no
        // recording.
        OffsetDateTime::now_local().unwrap_or_else(|_| OffsetDateTime::now_utc()),
        settings.format,
    );
    let running = recording::Recording::start(
        backend,
        recording.mixer.as_ref(),
        &path,
        backend.frame_rate(),
        settings,
    )?;
    recording.running = Some(running);
    println!("recording to {}", path.display());
    Ok(Instant::now())
}

/// The settings to record with, which are the stored ones unless the encoder
/// they name cannot be opened here.
///
/// The default is `Nvenc`, and it is a good default — but it is wrong on
/// every machine without an NVIDIA GPU, which is where the first Record press
/// would otherwise fail with nothing on screen but an error. So the encoder
/// falls through to the best one that did open.
///
/// The stored choice is not rewritten. Someone who picked NVENC on the
/// machine that has it should still find it selected after recording once on
/// a laptop that does not, rather than having their setting quietly replaced
/// by whatever that laptop could manage.
fn usable_settings(
    backend: &Backend,
    audio_codecs: &[crate::settings::RecordingAudioCodec],
    settings: &crate::settings::RecordingSettings,
) -> crate::settings::RecordingSettings {
    let mut settings = settings.clone();

    // The audio codec first, and on its own terms: a build without libopus
    // should still record, with sound, on the codec it does have — and so
    // should a mix at a rate libopus cannot take.
    if !audio_codecs.contains(&settings.audio_codec)
        && let Some(codec) = crate::settings::RecordingAudioCodec::best_of(audio_codecs)
    {
        eprintln!(
            "{} cannot be opened here; recording audio with {} instead",
            settings.audio_codec.label(),
            codec.label()
        );
        settings.audio_codec = codec;
    }

    let settings = &settings;
    let available = backend.available_encoders();
    if available.contains(&settings.encoder) {
        return settings.clone();
    }
    let Some(encoder) = crate::settings::RecordingEncoder::best_of(available) else {
        // Nothing opened at all. Recording with what was asked for will fail
        // and say why, which is better than failing with a substitution the
        // caller did not make.
        return settings.clone();
    };
    eprintln!(
        "{} cannot be opened here; recording with {} instead",
        settings.encoder.label(),
        encoder.label()
    );
    crate::settings::RecordingSettings {
        encoder,
        ..settings.clone()
    }
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

/// Deliberately not boxed. The lint measures the space every variant costs,
/// which here is a couple of hundred bytes times the number of SceneItems in
/// one Scene — nothing — against an allocation per open Source and a
/// dereference on every reconcile pass, which is the map's whole job.
#[allow(clippy::large_enum_variant)]
enum SourceState {
    Open(OpenSource),
    /// Opening failed once and will not be retried.
    ///
    /// A retry loop here would reopen the portal dialog on every snapshot,
    /// which is a stream of modal windows rather than an error message.
    Failed,
    /// Opened cleanly, and the thing it captures is not here right now.
    ///
    /// Only a Window Capture reaches this: a window is closed and reopened
    /// as a matter of course, so its absence is a state rather than a
    /// failure. Unlike `Failed` it is looked at again — see `retry_missing`
    /// — and nothing was opened, so there is nothing holding a dialog or a
    /// device while it waits.
    Missing,
    /// Not running, and nothing here may open it again.
    ///
    /// The window a Window Capture was showing has closed, or opening it did
    /// not work — and on this platform opening one means the portal's picker,
    /// a modal dialog over whatever the user is doing. So this is where such a
    /// Source stops: the Sources list says it is disconnected, and it comes
    /// back only when someone asks for it, through
    /// `EngineManager::reopen_source`.
    ///
    /// The distinction from `Missing` is who pays for the look:
    /// `WindowCaptureTarget::can_be_reopened_silently` is what decides which
    /// of the two a closed window lands in.
    Disconnected,
    /// A media file that played to its end without looping.
    ///
    /// Not a failure and not `Disconnected`: the Source did what it was told
    /// and there is nothing to recover from. It is a state of its own because
    /// staying `Open` would be a lie the rest of this cannot see through —
    /// the layer is already gone, the mixer input would sit registered and
    /// silent, and the Sources list would say nothing at all.
    Ended,
}

/// How long a `Missing` Source waits before it is looked for again.
///
/// The look enumerates every top-level window, so it is not something to do
/// on every idle tick; a second is well under what anyone notices between
/// bringing a window back and seeing it in the Scene.
const MISSING_RETRY: Duration = Duration::from_secs(1);

/// Opens one Scene item, turning both kinds of "no Source" into a state.
fn open_item(
    backend: &Backend,
    project: Option<&ProjectDispatcher>,
    mixer: Option<&media_pp::elements::MixerHandle>,
    item: &SceneItemSnapshot,
    layer: VideoLayer,
) -> SourceState {
    match backend.open_source(item, layer, backend.frame_rate(), mixer) {
        // Nothing to open yet, and nothing wrong. Looked at again on the next
        // pass rather than reported.
        Ok(None) => SourceState::Missing,
        Ok(Some(source)) => {
            // The portal may hand back a different token than the one it was
            // given. Keeping the old one would mean prompting on every launch,
            // which is the thing persisting it was for.
            if let (Some(project), Some(token)) = (project, source.refreshed_token.clone()) {
                project.dispatch(ProjectCommand::Source(SourceCommand::SetRestoreToken(
                    item.id, token,
                )));
            }
            SourceState::Open(source)
        }
        Err(error) => {
            eprintln!("could not open \"{}\": {error}", item.name);
            // A cancelled picker arrives here as an error, and it is an answer
            // rather than a fault: the user was asked and said not now. So a
            // Source that has to be asked for is left disconnected — offered
            // again by the Sources list — instead of failed, which nothing
            // ever reopens.
            if needs_asking(item) {
                SourceState::Disconnected
            } else {
                SourceState::Failed
            }
        }
    }
}

/// Tells the UI which Sources are not producing a picture, and why.
///
/// Stored only on a change: the Sources list reads this on every pass, and
/// replacing the map each time would hand it a new allocation a second for an
/// answer that is almost always the same one — usually empty.
fn publish_source_status(published: &Published, open: &HashMap<SceneItemId, SourceState>) {
    let status: HashMap<SceneItemId, SourceStatus> = open
        .iter()
        .filter_map(|(id, state)| {
            let status = match state {
                SourceState::Open(_) => return None,
                SourceState::Ended => SourceStatus::Ended,
                // Failed, Missing and Disconnected are one thing to a reader:
                // it is not showing, and the engine is not going to fix it by
                // itself. Which of the three it is decides what this side
                // does next, not what the list says.
                _ => SourceStatus::Disconnected,
            };
            Some((*id, status))
        })
        .collect();
    let unchanged = match published.source_status.load_full() {
        Some(current) => *current == status,
        // Nothing published yet, which an empty map says as well as `None`
        // does — and the first pass has nothing to correct.
        None => status.is_empty(),
    };
    if unchanged {
        return;
    }
    published.source_status.store(Some(Arc::new(status)));
}

/// Whether opening this item's Source would interrupt whoever is at the
/// screen, so that it must be asked for rather than attempted.
///
/// Only a Window Capture can answer yes, and only where its target is one the
/// portal owns — see `WindowCaptureTarget::can_be_reopened_silently`.
fn needs_asking(item: &SceneItemSnapshot) -> bool {
    match &item.settings {
        SourceSettings::WindowCapture(settings) => !settings.target.can_be_reopened_silently(),
        _ => false,
    }
}

/// Puts a Window Capture whose window has since closed back to `Missing`.
///
/// A window closing ends the capture: the Source stops, the compositor drops
/// the layer, and the pipeline is finished. Nothing tells the engine that, so
/// it asks — and once it knows, the Source is stopped and forgotten so that
/// `retry_missing` can open it again when the window comes back. Only a
/// Window Capture is asked: it is the one kind whose target is expected to
/// come and go.
fn notice_closed_windows(
    backend: &Backend,
    open: &mut HashMap<SceneItemId, SourceState>,
    snapshot: &SourcesSnapshot,
) {
    for item in &snapshot.items {
        if item.kind != SourceKind::WindowCapture {
            continue;
        }
        let Some(SourceState::Open(source)) = open.get(&item.id) else {
            continue;
        };
        if !source.source.ended() {
            continue;
        }
        source.source.stop();
        backend.remove_source(&source.name);
        // Whether the engine may go looking by itself, or has to wait to be
        // asked.
        let state = if needs_asking(item) {
            SourceState::Disconnected
        } else {
            SourceState::Missing
        };
        open.insert(item.id, state);
    }
}

/// Notices a media file that has played to its end.
///
/// Nothing tells the engine that either, so it asks, the same way it asks
/// about a closed window — and only about media files, because every other
/// kind here is live and its pipeline ending means something went wrong
/// rather than something finished.
///
/// The Source is stopped once noticed. That is not tidying: `Stop` is what
/// takes its input off the audio mixer, which an `Eos` alone leaves
/// registered and silent, so a finished file would otherwise keep a channel
/// in the Audio Mixer dock for as long as its SceneItem existed.
///
/// It is not reopened. Playing once is what a file that is not looping was
/// asked to do, and starting it again by itself would make the setting
/// meaningless.
fn notice_ended_media(
    backend: &Backend,
    open: &mut HashMap<SceneItemId, SourceState>,
    snapshot: &SourcesSnapshot,
) {
    for item in &snapshot.items {
        if item.kind != SourceKind::MediaFile {
            continue;
        }
        let Some(SourceState::Open(source)) = open.get(&item.id) else {
            continue;
        };
        if !source.source.ended() {
            continue;
        }
        source.source.stop();
        backend.remove_source(&source.name);
        open.insert(item.id, SourceState::Ended);
    }
}

/// Looks again for whatever a `Missing` Source captures.
///
/// This runs off the idle tick rather than off a Scene change: a window that
/// is closed and reopened while the user does nothing else in the app
/// produces no command at all, so waiting for one would leave the Source
/// blank until something unrelated happened to move.
fn retry_missing(
    backend: &Backend,
    project: Option<&ProjectDispatcher>,
    mixer: Option<&media_pp::elements::MixerHandle>,
    open: &mut HashMap<SceneItemId, SourceState>,
    snapshot: &SourcesSnapshot,
) {
    if !open
        .values()
        .any(|state| matches!(state, SourceState::Missing))
    {
        return;
    }
    let count = snapshot.items.len();
    for (index, item) in snapshot.items.iter().enumerate() {
        if !matches!(open.get(&item.id), Some(SourceState::Missing)) {
            continue;
        }
        let layer = layer_for(item, item.transform, (count - index) as i32);
        let state = open_item(backend, project, mixer, item, layer);
        open.insert(item.id, state);
    }
}

/// Brings the running Sources in line with what the project now holds.
fn reconcile(
    backend: &Backend,
    project: Option<&ProjectDispatcher>,
    mixer: Option<&media_pp::elements::MixerHandle>,
    open: &mut HashMap<SceneItemId, SourceState>,
    snapshot: &SourcesSnapshot,
) {
    let count = snapshot.items.len();
    for (index, item) in snapshot.items.iter().enumerate() {
        // The snapshot is ordered front-most first, and the compositor draws
        // larger z later, so the two run opposite ways.
        let layer = layer_for(item, item.transform, (count - index) as i32);
        match open.get_mut(&item.id) {
            Some(SourceState::Open(source)) => {
                let _ = source.layer.set_layer(layer);
                refresh_pushed(source, item);
                refresh_media_file(source, item);
            }
            Some(SourceState::Failed | SourceState::Disconnected | SourceState::Ended) => {}
            Some(SourceState::Missing) | None => {
                let state = open_item(backend, project, mixer, item, layer);
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
    use crate::domain::WindowCaptureTarget;

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

    fn window_item(id: i64, target: WindowCaptureTarget) -> SceneItemSnapshot {
        SceneItemSnapshot {
            id: SceneItemId(id),
            name: "Window Capture".into(),
            kind: SourceKind::WindowCapture,
            settings: SourceSettings::WindowCapture(crate::domain::WindowCaptureSettings {
                target,
                size_hint: None,
            }),
            source_size: [1280.0, 720.0],
            transform: Transform::default(),
            crop: crate::domain::Crop::default(),
            visible: true,
            locked: false,
        }
    }

    fn published() -> Published {
        Published {
            frame: Arc::new(ArcSwapOption::empty()),
            active_fps: Arc::new(AtomicU32::new(0)),
            recording_since: Arc::new(ArcSwapOption::empty()),
            recording_paused_at: Arc::new(ArcSwapOption::empty()),
            source_status: Arc::new(ArcSwapOption::empty()),
            encoders: Arc::new(ArcSwapOption::empty()),
            audio_codecs: Arc::new(ArcSwapOption::empty()),
            recording_error: Arc::new(ArcSwapOption::empty()),
        }
    }

    /// The whole of option D rests on this one answer: a window the engine
    /// can search for is searched for, and a window the portal owns is left
    /// alone until someone asks. Getting it backwards is either a dialog
    /// every second or a Source that never comes back.
    #[test]
    fn only_a_window_the_portal_owns_has_to_be_asked_for() {
        assert!(
            !needs_asking(&window_item(
                1,
                WindowCaptureTarget::Window {
                    process: "firefox".into(),
                    title: "obs-rs".into(),
                }
            )),
            "a named window is found by looking, which costs no one anything"
        );
        assert!(
            needs_asking(&window_item(
                2,
                WindowCaptureTarget::Portal {
                    restore_token: Some("token".into()),
                }
            )),
            "a portal window can only be reopened through its picker"
        );

        let mut colour = window_item(
            3,
            WindowCaptureTarget::Portal {
                restore_token: None,
            },
        );
        colour.kind = SourceKind::Color;
        colour.settings = SourceSettings::None;
        assert!(
            !needs_asking(&colour),
            "nothing but a Window Capture has a picker behind it"
        );
    }

    /// Two claims at once, because the second is what makes the first cheap:
    /// the map says which Sources are dark and why, and it is replaced only
    /// when that answer moves. The UI reads it on every pass.
    #[test]
    fn the_status_map_names_the_dark_sources_and_holds_still() {
        let published = published();
        let mut open = HashMap::new();

        publish_source_status(&published, &open);
        assert!(
            published.source_status.load_full().is_none(),
            "nothing has gone wrong yet, so there is nothing to say"
        );

        open.insert(SceneItemId(1), SourceState::Disconnected);
        open.insert(SceneItemId(2), SourceState::Missing);
        // A file that played out is dark for a different reason, and says so.
        open.insert(SceneItemId(3), SourceState::Ended);
        publish_source_status(&published, &open);
        let first = published
            .source_status
            .load_full()
            .expect("a dark Source must be published");
        assert_eq!(
            *first,
            HashMap::from([
                (SceneItemId(1), SourceStatus::Disconnected),
                (SceneItemId(2), SourceStatus::Disconnected),
                (SceneItemId(3), SourceStatus::Ended),
            ])
        );

        publish_source_status(&published, &open);
        let again = published
            .source_status
            .load_full()
            .expect("still published");
        assert!(
            Arc::ptr_eq(&first, &again),
            "an unchanged map must not be replaced"
        );

        open.remove(&SceneItemId(1));
        open.remove(&SceneItemId(2));
        open.remove(&SceneItemId(3));
        publish_source_status(&published, &open);
        assert_eq!(
            *published
                .source_status
                .load_full()
                .expect("the recovery has to be published too"),
            HashMap::new(),
            "a Source that came back must stop being listed"
        );
    }
}
