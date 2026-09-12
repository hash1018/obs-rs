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
pub(in crate::engine) mod load;
mod output;
mod preview;
mod status;
pub(in crate::engine) mod trouble;

pub use preview::CompositeFrame;
mod source;

pub use audio::{AudioManager, METER_INTERVAL, MeterWake};
/// What an output encodes with — read off the settings, and the only thing
/// the encoders are told about which kind of output they serve.
pub use output::OutputEncoding;
pub use trouble::Trouble;

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
use media_pp::elements::{VideoFit, VideoLayer, VideoRect, VideoSourceRect};

use crate::domain::{Crop, SceneCanvas, SceneItemId, SourceSettings, Transform};
use crate::project::{ProjectCommand, ProjectDispatcher, SourceCommand};
use crate::snapshots::{Role, SceneItemSnapshot, SourceStatus, SourcesSnapshot};
use output::{Broadcast, BroadcastRequest, OutputKind, OutputState, describe, start_recording};

use backend::{Backend, BackendError};
use source::{
    OpenSource, PushedContent, filters, push_content, refresh_media_file, refresh_pushed,
};

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
    /// One Source has finished opening, however it came out. Sent by the
    /// opener thread rather than by the UI — see [`SourceOpener`].
    Opened(Box<Opened>),
    /// One item's Transform mid-gesture, which the project does not hold yet.
    Dragging(SceneItemId, Transform, Crop),
    /// One filter's settings mid-gesture, for the same reason `Dragging`
    /// exists: a threshold is tuned by watching the picture, and a project
    /// told once per frame would write a transaction per frame of the drag.
    FilterSettings(
        SceneItemId,
        crate::domain::FilterId,
        crate::domain::FilterSettings,
    ),
    /// A Drawing's strokes mid-gesture, for the same reason `Dragging` exists:
    /// the mark has to appear under the pointer, and the project is not told
    /// until the pointer comes up. Carries the whole list rather than the one
    /// new stroke, because rasterizing is done from the list either way.
    Drawing(SceneItemId, Vec<crate::domain::Stroke>),
    /// A Color Source.s colour while the picker is still held, for the same
    /// reason `Drawing` exists: the picture has to follow the pointer, and
    /// the project is told once when it is let go.
    Colour(SceneItemId, [u8; 4]),
    /// A Text Source's settings while its field is still being typed into,
    /// for the same reason `Colour` exists. Carries all of them, because
    /// what is redrawn is drawn from all of them.
    Text(SceneItemId, crate::domain::TextSourceSettings),
    /// A media file Source's gain while the fader is still held — the audio
    /// counterpart of `Colour`, and on this thread rather than the audio one
    /// because a file's fader belongs to its own pipeline.
    MediaGain(SceneItemId, f32),
    /// Move one media file Source to a position in its own file.
    ///
    /// Not a project edit: where a clip is playing from is not something to
    /// record, the way a Transform or a colour is. Scrubbing it is closer to
    /// looking at it than to changing it.
    MediaSeek(SceneItemId, Duration),
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
    StartStreaming,
    StopStreaming,
    /// What a connect attempt came out as, from the thread that made it.
    /// Boxed because an `Output` is large and this variant is rare — see
    /// `Opened`, which is boxed for the same reason.
    BroadcastOpened(Box<Result<output::Output, BackendError>>),
    StreamingSettings(Box<crate::settings::StreamingSettings>),
}

/// What the engine is started with, as opposed to what it is told afterwards
/// over [`EngineCommand`]. Grouped because they travel together and because
/// `run` had collected more parameters than anyone can read at a glance.
struct EngineSetup {
    size: [u32; 2],
    project: Option<ProjectDispatcher>,
    recording: OutputState,
    /// What the audio thread reports its own pipelines' failures on.
    ///
    /// `None` on a machine whose audio never started, which is also a
    /// machine with no mix for an output's audio track to fail on.
    audio_troubles: Option<mpsc::Receiver<Trouble>>,
    /// What the audio thread's own outputs are costing — see `load`.
    audio_load: Arc<ArcSwapOption<load::Load>>,
    /// For the meters of Sources that bring their own sound — see
    /// `AudioLink::meter_wake`.
    meter_wake: MeterWake,
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
    /// What share of the last second the outputs spent holding up whatever
    /// feeds them — `f32` bits, so the UI reads it without a lock.
    ///
    /// Published from the engine loop rather than computed by the UI: it is
    /// the difference between two readings, and only the loop knows when it
    /// took them.
    output_load: Arc<AtomicU32>,
    /// What every named part of the graph is doing — see `load::Reading`.
    stats: Arc<ArcSwapOption<crate::snapshots::StatsSnapshot>>,
    /// Whether a dropped broadcast is being retried.
    ///
    /// A flag beside the clock rather than a third instant: what the status
    /// bar needs to say is that this is not simply off, and the interval it
    /// is waiting out is the settings' and not news.
    streaming_reconnecting: Arc<AtomicBool>,
    /// When the running broadcast started, or `None` when none is.
    ///
    /// Its own slot rather than a flag beside the recording's: the two run
    /// independently and each has its own clock, and a viewer wants to know
    /// how long the channel has been live whether or not anything is also
    /// being written to disk.
    streaming_since: Arc<ArcSwapOption<Instant>>,
    /// Why the last attempt to start a broadcast failed, if it did. Cleared
    /// when the next attempt is made, exactly as the recording's is.
    streaming_error: Arc<ArcSwapOption<String>>,
    /// The SceneItems whose Source is not running, and why: a window that has
    /// closed, one that never opened, or a file that played out. The Sources
    /// list says so beside them, which is the only thing that explains an
    /// item drawing nothing.
    source_status: Arc<ArcSwapOption<HashMap<SceneItemId, SourceStatus>>>,
    /// What each playing media file measures about itself: its level, and
    /// where it has reached.
    ///
    /// The audio thread has its own `Levels` for the devices and replaces it
    /// wholesale whenever it rebuilds its graph. This is the second half, and
    /// it is published separately for exactly that reason: one side must not
    /// wipe the other's counters by getting on with its own work. The Audio
    /// Mixer dock reads both levels and draws them as one row of channels.
    ///
    /// The map changes only when a Source opens or closes; the numbers inside
    /// it change every buffer, which is what the atomics are for.
    media_meters: Arc<ArcSwapOption<HashMap<SceneItemId, Arc<source::MediaMeters>>>>,
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

/// What the engine is handed at construction about where output goes.
///
/// One argument rather than two, which is also what keeps `spawn` inside
/// what clippy will look at without complaint — but the reason they are
/// together is that they are the same thing said twice: what the next
/// recording is written as, and what the next broadcast is published as.
/// Everything the engine takes from the audio side at construction.
///
/// One argument rather than three, because they are one thing: the audio
/// thread's mixer, its monitor mix, and the channel it reports failures on.
/// All three are `Option` for the same reason — a machine whose audio never
/// started has none of them, and records video only.
pub struct AudioLink {
    /// Where an output's audio track attaches — see `AudioManager::mixer`.
    pub mixer: Option<(
        media_pp::elements::TeeHandle,
        media_pp::elements::MixerHandle,
    )>,
    /// The mix that is played back, which unlike the one above comes and
    /// goes as a monitoring endpoint is chosen and taken away.
    pub monitor: Arc<ArcSwapOption<media_pp::elements::MixerHandle>>,
    /// What that thread reports its own pipelines' failures on — see
    /// `trouble`.
    pub troubles: Option<mpsc::Receiver<Trouble>>,
    /// The same wake the audio thread's meters use, for the meters of media
    /// files and streams. One between them, so the limit it keeps is on the
    /// window rather than on each half — see [`MeterWake`].
    pub meter_wake: MeterWake,
    /// What its own outputs are costing, republished each pass — see
    /// [`load`].
    pub load: Arc<ArcSwapOption<load::Load>>,
}

pub struct OutputSettings {
    pub recording: crate::settings::RecordingSettings,
    pub streaming: crate::settings::StreamingSettings,
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
    /// When the running broadcast started — see `Published::streaming_since`.
    streaming_since: Arc<ArcSwapOption<Instant>>,
    streaming_error: Arc<ArcSwapOption<String>>,
    /// Whether a dropped broadcast is being retried — see
    /// `Published::streaming_reconnecting`.
    streaming_reconnecting: Arc<AtomicBool>,
    /// What the outputs are costing — see `Published::output_load`.
    output_load: Arc<AtomicU32>,
    /// What every named part of the graph is doing — see `load::Reading`.
    stats: Arc<ArcSwapOption<crate::snapshots::StatsSnapshot>>,
    /// The SceneItems drawing nothing — see `Published::source_status`.
    source_status: Arc<ArcSwapOption<HashMap<SceneItemId, SourceStatus>>>,
    /// What each playing media file measures — see `Published::media_meters`.
    media_meters: Arc<ArcSwapOption<HashMap<SceneItemId, Arc<source::MediaMeters>>>>,
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
        // The two output settings as one, because they arrive together and
        // for the same reason: handed over at construction rather than sent
        // afterwards, so an output started before the Settings dialog is
        // ever opened uses what the user saved rather than the defaults.
        outputs: OutputSettings,
        audio: AudioLink,
        wake_ui: impl Fn() + Send + Sync + 'static,
    ) -> std::io::Result<Self> {
        let AudioLink {
            mixer,
            monitor,
            troubles: audio_troubles,
            load: audio_load,
            meter_wake,
        } = audio;
        let size = [canvas.width as u32, canvas.height as u32];
        let frame = Arc::new(ArcSwapOption::empty());
        let active_fps = Arc::new(AtomicU32::new(0));
        let recording_since = Arc::new(ArcSwapOption::empty());
        let recording_paused_at = Arc::new(ArcSwapOption::empty());
        let stop = Arc::new(AtomicBool::new(false));
        let recording_error = Arc::new(ArcSwapOption::empty());
        let streaming_since = Arc::new(ArcSwapOption::empty());
        let streaming_error = Arc::new(ArcSwapOption::empty());
        let streaming_reconnecting = Arc::new(AtomicBool::new(false));
        let output_load = Arc::new(AtomicU32::new(0));
        let stats = Arc::new(ArcSwapOption::empty());
        let source_status = Arc::new(ArcSwapOption::empty());
        let media_meters = Arc::new(ArcSwapOption::empty());
        let encoders = Arc::new(ArcSwapOption::empty());
        let audio_codecs = Arc::new(ArcSwapOption::empty());
        let (commands, command_rx) = mpsc::channel();

        let worker = thread::Builder::new().name("engine".to_owned()).spawn({
            let frame = Arc::clone(&frame);
            let active_fps = Arc::clone(&active_fps);
            let recording_since = Arc::clone(&recording_since);
            let recording_paused_at = Arc::clone(&recording_paused_at);
            let recording_error = Arc::clone(&recording_error);
            let streaming_since = Arc::clone(&streaming_since);
            let streaming_error = Arc::clone(&streaming_error);
            let streaming_reconnecting = Arc::clone(&streaming_reconnecting);
            let output_load = Arc::clone(&output_load);
            let stats = Arc::clone(&stats);
            let source_status = Arc::clone(&source_status);
            let media_meters = Arc::clone(&media_meters);
            let encoders = Arc::clone(&encoders);
            let audio_codecs = Arc::clone(&audio_codecs);
            let stop = Arc::clone(&stop);
            // The engine's own way back into its queue, for the thread that
            // opens Sources — see [`SourceOpener`].
            let replies = commands.clone();
            move || {
                let published = Published {
                    frame,
                    active_fps,
                    recording_since,
                    recording_paused_at,
                    recording_error,
                    streaming_since,
                    streaming_error,
                    streaming_reconnecting,
                    output_load,
                    stats,
                    source_status,
                    media_meters,
                    encoders,
                    audio_codecs,
                };
                let setup = EngineSetup {
                    size,
                    project,
                    audio_troubles,
                    audio_load,
                    meter_wake,
                    recording: OutputState {
                        settings: outputs.recording,
                        streaming: outputs.streaming,
                        mixer,
                        monitor,
                        // Filled in once the probe has run — see `run`.
                        audio_codecs: Vec::new(),
                        running: None,
                        broadcast: Broadcast::Off,
                    },
                };
                if let Err(error) = run(
                    render_state,
                    setup,
                    published,
                    command_rx,
                    replies,
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
            recording_since,
            recording_paused_at,
            recording_error,
            streaming_since,
            streaming_error,
            streaming_reconnecting,
            output_load,
            stats,
            source_status,
            media_meters,
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

    /// Redraws a Text Source while its field is still being typed into, for
    /// the same reason [`Self::set_drawing_strokes`] exists.
    pub fn set_source_text(&self, item: SceneItemId, settings: crate::domain::TextSourceSettings) {
        let _ = self.commands.send(EngineCommand::Text(item, settings));
    }

    /// Moves one layer while the pointer is still down.
    ///
    /// The project database only learns the Transform when the gesture ends,
    /// which is correct — a drag is not a series of edits. But the compositor
    /// would then show the item where it used to be until the pointer is
    /// released, with the gizmo somewhere else entirely, so the layer follows
    /// the gesture directly and the snapshot confirms it afterwards.
    pub fn set_dragging_transform(&self, item: SceneItemId, transform: Transform, crop: Crop) {
        let _ = self
            .commands
            .send(EngineCommand::Dragging(item, transform, crop));
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

    /// Begin publishing to the server the settings name.
    pub fn start_streaming(&self) {
        let _ = self.commands.send(EngineCommand::StartStreaming);
    }

    /// End the running broadcast.
    pub fn stop_streaming(&self) {
        let _ = self.commands.send(EngineCommand::StopStreaming);
    }

    /// What the *next* broadcast publishes to and how. Read when one starts,
    /// so changing this disturbs nothing that is running.
    pub fn set_streaming_settings(&self, settings: crate::settings::StreamingSettings) {
        let _ = self
            .commands
            .send(EngineCommand::StreamingSettings(Box::new(settings)));
    }

    /// How long the running broadcast has been live, or `None` when none is.
    ///
    /// Unlike a recording's, this has no paused span to subtract: a
    /// broadcast cannot be paused. Stopping it ends it.
    pub fn streaming(&self) -> Option<Duration> {
        let since = self.streaming_since.load_full()?;
        Some(since.elapsed())
    }

    /// Why the last attempt to start a broadcast failed, if it did.
    pub fn streaming_error(&self) -> Option<Arc<String>> {
        self.streaming_error.load_full()
    }

    /// Whether a dropped broadcast is waiting to be tried again.
    pub fn streaming_reconnecting(&self) -> bool {
        self.streaming_reconnecting.load(Ordering::Acquire)
    }

    /// What share of the last interval the outputs spent holding up the
    /// compositor and the mixer — see `engine::load`.
    pub fn output_load(&self) -> f32 {
        f32::from_bits(self.output_load.load(Ordering::Acquire))
    }

    /// What every named part of the graph is doing, or `None` before the
    /// first reading — see `engine::load::Reading`.
    pub fn stats(&self) -> Option<Arc<crate::snapshots::StatsSnapshot>> {
        self.stats.load_full()
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

    /// What this media file Source's meter reads, or `None` for one with no
    /// sound and for anything that is not a media file.
    ///
    /// Read from the atomic its own audio branch writes, so it costs no lock
    /// and no wait on the engine thread — the same arrangement the devices'
    /// meters have, and the same reason: a meter one frame stale is a meter,
    /// and one that blocks the graph to be current is not.
    pub fn media_peak_db(&self, item: SceneItemId) -> Option<f32> {
        let bits = self
            .media_meters
            .load_full()?
            .get(&item)?
            .peak
            .load(Ordering::Relaxed);
        (bits != 0).then(|| f32::from_bits(bits))
    }

    /// Where this media file Source has reached in its file, or `None` for
    /// one that is not open or has not produced a frame yet.
    ///
    /// A position in the file rather than on the wire: a looping Source's
    /// timestamps climb past the end, and what is taken off them is the
    /// demuxer's own account of how far they have been carried.
    pub fn media_position(&self, item: SceneItemId) -> Option<Duration> {
        let micros = self
            .media_meters
            .load_full()?
            .get(&item)?
            .position
            .load(Ordering::Relaxed);
        u64::try_from(micros).ok().map(Duration::from_micros)
    }

    /// Moves one media file Source to `target`, measured in its own file.
    ///
    /// Keyframe rather than accurate: this is a scrub bar, where landing
    /// promptly on roughly the right picture is the point and decoding
    /// forward to an exact frame is not. A Source that is not a media file,
    /// or is not open, is left alone.
    pub fn seek_media_file(&self, item: SceneItemId, target: Duration) {
        let _ = self.commands.send(EngineCommand::MediaSeek(item, target));
    }

    /// One media file Source's gain while the fader is still held, for the
    /// same reason `set_source_colour` exists: what is heard has to follow
    /// the pointer, and the project is told once when the gesture ends.
    pub fn set_media_gain_db(&self, item: SceneItemId, gain_db: f32) {
        let _ = self.commands.send(EngineCommand::MediaGain(item, gain_db));
    }

    /// One filter's settings while its slider is still held.
    ///
    /// Reaches the running element and nothing else; the project is told
    /// once, when the pointer comes up.
    pub fn set_filter_settings(
        &self,
        item: SceneItemId,
        filter: crate::domain::FilterId,
        settings: crate::domain::FilterSettings,
    ) {
        let _ = self
            .commands
            .send(EngineCommand::FilterSettings(item, filter, settings));
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
    // The other end of `commands`, for the opener thread to answer down — see
    // `SourceOpener`. Held here as well as by the manager, which is why this
    // loop leaves on the `stop` flag rather than on the channel closing.
    replies: mpsc::Sender<EngineCommand>,
    stop: &AtomicBool,
    wake_ui: impl Fn() + Send + Sync + 'static,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let EngineSetup {
        size,
        project,
        mut recording,
        audio_troubles,
        audio_load,
        meter_wake,
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
    let backend = Arc::new(Backend::start(
        &render_state,
        size,
        recording.settings.fps.max(1),
        // Never above what is being composited: a Preview asking for 30 of a
        // Scene made at 24 is asking for frames that do not exist.
        PREVIEW_FPS.min(recording.settings.fps.max(1)),
        publish,
        meter_wake,
    )?);

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
    recording.audio_codecs = output::available_audio_codecs(recording.mix_format());
    published
        .audio_codecs
        .store(Some(Arc::new(recording.audio_codecs.clone())));

    // Replies come back through the loop's own channel, so the opener needs a
    // way in — see [`SourceOpener`].
    let opener = SourceOpener::spawn(Arc::clone(&backend), replies.clone())?;
    let broadcasts = BroadcastOpener::spawn(Arc::clone(&backend), replies)?;
    let engine = Engine {
        backend: &backend,
        project: project.as_ref(),
        opener: &opener,
        broadcasts: &broadcasts,
    };

    let mut open = HashMap::new();
    let mut scene = SourcesSnapshot::default();
    let mut looked_for_missing = Instant::now();
    // The reading the next one is measured against — see `engine::load`.
    let mut last_load = (Instant::now(), load::Load::default());
    let mut reading = load::Reading::new();
    // `recording` is owned by this loop rather than shared: only the commands
    // below reach it, and a settings change arrives on the same channel a
    // start does, so one can never land half-way through a recording being
    // opened.
    while !stop.load(Ordering::Acquire) {
        match commands.recv_timeout(Duration::from_millis(100)) {
            Ok(command) => {
                let mut reconciled = apply_command(
                    &engine,
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
                        &engine,
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
                // Before the interval below, not after it: a clock has to be
                // redrawn as it moves, and this loop's timeout is what it
                // moves against. `refresh_pushed` compares what it would
                // draw against what it drew, so the pass costs a formatted
                // string per clock and nothing at all until a second turns
                // over.
                redraw_clocks(&mut open, &scene);
                // What the buses reported, and a retry whose wait is up.
                // Before the interval below for the reason the clocks are:
                // a broadcast that dropped a moment ago should not go on
                // being reported as live for the rest of a second.
                watch_outputs(&engine, &mut recording, &published, audio_troubles.as_ref());
                if looked_for_missing.elapsed() < MISSING_RETRY {
                    continue;
                }
                looked_for_missing = Instant::now();
                last_load = publish_output_load(&engine, &published, &audio_load, last_load);
                publish_stats(&engine, &published, &open, &scene, &mut reading);
                status::notice_closed_windows(&backend, &mut open, &scene);
                status::notice_ended_media(&backend, &mut open, &scene);
                status::notice_dropped_streams(&backend, &mut open, &scene);
                retry_missing(&engine, recording.mixer_handle(), &mut open, &scene);
            }
            Err(RecvTimeoutError::Disconnected) => break,
        }
        // Only where something may have opened or closed, which is what the
        // `continue`s above skip past: this is a comparison against what the
        // UI already holds, not something to do sixty times a second for an
        // answer that has not moved.
        status::publish_source_status(&published, &open);
        status::publish_media_meters(&published, &open);
    }

    // Joined before the Sources are stopped and well before the backend is:
    // whatever is being opened at this moment is being opened against that
    // backend. `engine` borrows the opener and is not used past here, which
    // is what lets this drop run at all.
    drop(opener);
    drop(broadcasts);

    for (_, state) in open.drain() {
        if let SourceState::Open(source) = state {
            source.source.stop();
        }
    }
    backend.stop();
    Ok(())
}

/// Asks the opener thread to connect, and records that it was asked.
///
/// Idempotent against a connect already in flight: pressing Start twice, or
/// a retry falling due while the previous attempt is still handshaking, must
/// not open two broadcasts to the same server.
fn request_broadcast(engine: &Engine<'_>, state: &mut OutputState, published: &Published) {
    if matches!(state.broadcast, Broadcast::Connecting | Broadcast::Live(_)) {
        return;
    }
    state.broadcast = Broadcast::Connecting;
    published
        .streaming_reconnecting
        .store(false, Ordering::Release);
    engine.broadcasts.request(BroadcastRequest {
        settings: state.streaming.clone(),
        fps: engine.backend.frame_rate(),
        mixer: state.mixer.clone(),
    });
}

/// Takes what the opener thread came back with.
///
/// A broadcast that arrives after Stop was pressed is stopped again rather
/// than shown: the user's answer is the newer one, and `Broadcast::wanted`
/// is what says so.
fn finish_broadcast(
    engine: &Engine<'_>,
    state: &mut OutputState,
    published: &Published,
    opened: Result<output::Output, BackendError>,
) {
    match opened {
        Ok(running) if state.broadcast.wanted() => {
            state.broadcast = Broadcast::Live(running);
            published
                .streaming_since
                .store(Some(Arc::new(Instant::now())));
            published
                .streaming_reconnecting
                .store(false, Ordering::Release);
            published.streaming_error.store(None);
        }
        Ok(running) => {
            // Stopped while it was connecting. It has a live connection and
            // two attached branches, so it has to be ended properly rather
            // than dropped.
            if let Err(error) = running.stop(engine.backend) {
                eprintln!("could not stop the broadcast that arrived late: {error}");
            }
        }
        Err(error) => {
            let reason = describe(error.as_ref());
            eprintln!("could not start streaming: {reason}");
            published.streaming_error.store(Some(Arc::new(reason)));
            broadcast_dropped(state, published);
        }
    }
}

/// Puts the broadcast into whichever state a failure leaves it in.
///
/// Retried where the settings ask for it, and given up where they do not.
/// Called both when a connect fails and when a live one drops, because the
/// two are the same thing to decide: there is no broadcast, and either
/// something will try again or nothing will.
fn broadcast_dropped(state: &mut OutputState, published: &Published) {
    published.streaming_since.store(None);
    match state.streaming.reconnect() {
        Some(interval) if state.broadcast.wanted() => {
            state.broadcast = Broadcast::Waiting {
                retry_at: Instant::now() + interval,
            };
            published
                .streaming_reconnecting
                .store(true, Ordering::Release);
        }
        _ => {
            state.broadcast = Broadcast::Off;
            published
                .streaming_reconnecting
                .store(false, Ordering::Release);
        }
    }
}

/// Acts on what the buses reported, and retries a broadcast whose wait is up.
///
/// Both on the idle tick, because both are things nothing else will mention:
/// a muxer that failed posted to a bus and carried on, and an interval that
/// elapsed is not an event at all.
fn watch_outputs(
    engine: &Engine<'_>,
    state: &mut OutputState,
    published: &Published,
    audio_troubles: Option<&mpsc::Receiver<Trouble>>,
) {
    let mut troubles = trouble::drain(engine.backend.preview.bus(), "compositor");
    if let Some(audio) = audio_troubles {
        troubles.extend(audio.try_iter());
    }
    for trouble in troubles {
        match trouble {
            Trouble::Broadcast(reason) => {
                // Both RTMP tracks write through one connection, so one
                // going takes both — and reports twice. The first report
                // takes the broadcast out of `Live`; the second finds
                // nothing to end, and must not be acted on again or it
                // would restart the interval the first one set.
                let Some(running) = state.broadcast.take_live() else {
                    continue;
                };
                // Ended rather than abandoned: the branches are still
                // attached to both `Tee`s and would go on feeding a muxer
                // that has stopped accepting anything.
                if let Err(error) = running.stop(engine.backend) {
                    eprintln!("could not end the dropped broadcast: {error}");
                }
                published.streaming_error.store(Some(Arc::new(reason)));
                broadcast_dropped(state, published);
            }
            Trouble::Recording(reason) => {
                published.recording_since.store(None);
                published.recording_paused_at.store(None);
                if let Some(running) = state.running.take()
                    && let Err(error) = running.stop(engine.backend)
                {
                    eprintln!("could not end the failed recording: {error}");
                }
                eprintln!("the recording stopped: {reason}");
                published.recording_error.store(Some(Arc::new(reason)));
            }
        }
    }

    if let Broadcast::Waiting { retry_at } = state.broadcast
        && Instant::now() >= retry_at
    {
        request_broadcast(engine, state, published);
    }
}

/// Works out what the outputs cost over the interval just ended, and
/// publishes it.
///
/// Both pipelines, added together: an output's video branch is on the
/// compositor's and its audio on the audio thread's, and either can be the
/// one that is behind. The audio side arrives already read — see
/// `AudioManager::load`.
///
/// Answers the reading to measure the next interval against.
fn publish_output_load(
    engine: &Engine<'_>,
    published: &Published,
    audio_load: &ArcSwapOption<load::Load>,
    last: (Instant, load::Load),
) -> (Instant, load::Load) {
    let now = Instant::now();
    let total = load::Load::read(&engine.backend.preview.stats()).merge(
        audio_load
            .load_full()
            .map_or_else(load::Load::default, |load| *load),
    );
    let (_, before) = last;
    let share = total.since(before).pressure();
    published
        .output_load
        .store(share.to_bits(), Ordering::Release);
    (now, total)
}

/// Reads every pipeline and publishes what the Stats dock draws.
///
/// Every pipeline, because they are not one: the compositor has its own, the
/// mixer has its own, and each Source has its own again. What ties them
/// together is the names — which element belongs to which of the few things
/// a person would act on.
fn publish_stats(
    engine: &Engine<'_>,
    published: &Published,
    open: &HashMap<SceneItemId, SourceState>,
    scene: &SourcesSnapshot,
    reading: &mut load::Reading,
) {
    use crate::snapshots::Subject;

    let compositor = engine.backend.preview.stats();
    // Held for as long as the borrows below, since `take` reads through them.
    let scene_sources: Vec<_> = scene
        .items
        .iter()
        .filter_map(|item| {
            let SourceState::Open(open) = open.get(&item.id)? else {
                return None;
            };
            Some((
                source::input_name(item),
                item.name.clone(),
                open.source.stats()?,
            ))
        })
        .collect();

    let sources: HashMap<&str, &str> = scene_sources
        .iter()
        .map(|(element, name, _)| (element.as_str(), name.as_str()))
        .collect();

    let mut pipelines = vec![("preview", load::elements(&compositor))];
    pipelines.extend(
        scene_sources
            .iter()
            .map(|(element, _, stats)| (element.as_str(), load::elements(stats))),
    );
    let snapshot = reading.take(&pipelines, |element| match element.name {
        // The compositor takes nothing in — it is its pipeline's source, and
        // what it made is on its output pad. What compositing costs is not
        // an element's `busy` at all: it happens in the thread that drives
        // it rather than in a call into it.
        "preview-compositor" => Some((Subject::Compositor, Role::Throughput)),
        // Every element of an output's branch, by the names
        // `OutputKind::prefix` already decides — the same naming `trouble`
        // and `Load` read. The muxer at the end says how much reached the
        // file or the wire; the queue at the head says what the encoding
        // cost and how close it is to blocking.
        name if is_an_output(name) => {
            let kind = if name.starts_with(OutputKind::Recording.prefix()) {
                Subject::Recording
            } else {
                Subject::Broadcast
            };
            Some((kind, output_role(element)))
        }
        // A Source's own name belongs to two of its elements — the head
        // that produces and the input it feeds the compositor through. The
        // input is the one worth reporting: its count is frames that
        // actually reached the Canvas, which is the question a Source
        // raises. It is the one with nothing downstream of it.
        name if element.pushed.is_none() => sources
            .get(name)
            .map(|name| (Subject::Source((*name).to_owned()), Role::Throughput)),
        _ => None,
    });
    published.stats.store(Some(Arc::new(snapshot)));
}

/// Whether an element belongs to one of the outputs.
fn is_an_output(name: &str) -> bool {
    name.starts_with(OutputKind::Recording.prefix())
        || name.starts_with(OutputKind::Broadcast.prefix())
}

/// What part of an output's row one of its elements supplies.
///
/// Its queue, where it has one: an output's queues fill and then block
/// because something below them is behind, and a queue's worker drives
/// every stage after it, so its time is what the whole branch costs.
/// Otherwise its count, which at the end of the branch is what was written.
fn output_role(element: &load::Element<'_>) -> Role {
    if element.queue.is_some() {
        Role::Cost
    } else {
        Role::Throughput
    }
}

/// Redraws every Source that follows the clock rather than an edit.
///
/// The Sources that do are a running Text Source, and today that is all —
/// see [`source::redraws_with_the_clock`]. Everything else this loop holds is
/// redrawn by a Scene change, which arrives as a command rather than as time
/// passing.
fn redraw_clocks(open: &mut HashMap<SceneItemId, SourceState>, scene: &SourcesSnapshot) {
    for item in &scene.items {
        if !source::redraws_with_the_clock(item) {
            continue;
        }
        if let Some(SourceState::Open(source)) = open.get_mut(&item.id) {
            refresh_pushed(source, item);
        }
    }
}

/// Opens Sources on a thread of its own.
///
/// Opening one is neither quick nor bounded. A portal capture waits on a
/// dialog the user may leave standing, a file comes off a disk that may have
/// spun down, and a network stream waits out a connect timeout — five seconds
/// of nothing, for a camera that is switched off. On the engine loop each of
/// those is the whole engine stopped for as long as it takes: no layer moves,
/// no recording starts, no command is read.
///
/// So the loop asks, and hears back through the channel it already reads.
/// A reply arrives as [`EngineCommand::Opened`] and is applied where every
/// other change is, which is what keeps the state machine in one place.
///
/// One thread rather than one per request, deliberately: opening was
/// sequential before and two portal captures asked for at once would
/// otherwise put two pickers on the screen together.
struct SourceOpener {
    requests: mpsc::Sender<OpenRequest>,
    /// Not joined on drop — see [`SourceOpener::drop`].
    worker: Option<JoinHandle<()>>,
}

/// One Source to open, as it was asked for.
struct OpenRequest {
    item: Box<SceneItemSnapshot>,
    layer: VideoLayer,
    fps: u32,
    /// A clone rather than a borrow: the mixer outlives one open, and the
    /// thread cannot hold a reference into the engine loop's own state.
    mixer: Option<media_pp::elements::MixerHandle>,
}

/// What an open came out as, on its way back to the loop that asked.
///
/// The item comes back with it because the loop needs what was asked for to
/// make sense of the answer — the name to report, and the settings that
/// decide whether a refusal is a state or a failure.
pub(crate) struct Opened {
    item: Box<SceneItemSnapshot>,
    result: Result<Option<OpenSource>, BackendError>,
}

impl SourceOpener {
    fn spawn(backend: Arc<Backend>, replies: mpsc::Sender<EngineCommand>) -> std::io::Result<Self> {
        let (requests, incoming) = mpsc::channel::<OpenRequest>();
        let worker = thread::Builder::new()
            .name("source-opener".to_owned())
            .spawn(move || {
                while let Ok(request) = incoming.recv() {
                    let result = backend.open_source(
                        &request.item,
                        request.layer,
                        request.fps,
                        request.mixer.as_ref(),
                    );
                    let opened = Opened {
                        item: request.item,
                        result,
                    };
                    let Err(undelivered) = replies.send(EngineCommand::Opened(Box::new(opened)))
                    else {
                        continue;
                    };
                    // The engine has gone while this was opening. What came
                    // back is running and nothing else holds it, so it is
                    // stopped here rather than dropped on the floor.
                    if let EngineCommand::Opened(opened) = undelivered.0
                        && let Ok(Some(source)) = opened.result
                    {
                        source.source.stop();
                        backend.remove_source(&source.name);
                    }
                    break;
                }
            })?;
        Ok(Self {
            requests,
            worker: Some(worker),
        })
    }

    fn request(&self, request: OpenRequest) -> Result<(), mpsc::SendError<OpenRequest>> {
        self.requests.send(request)
    }
}

/// Connects broadcasts on a thread of its own.
///
/// A second thread rather than a queue on [`SourceOpener`], because what
/// each waits for is unbounded and unrelated. A portal picker left standing
/// would hold a reconnect behind it for as long as nobody answered it — and
/// a reconnect against a server that is down would hold up every camera in
/// the Scene for ten seconds at a time. Neither should be able to delay the
/// other, and the only thing that guarantees it is not sharing a queue.
struct BroadcastOpener {
    requests: mpsc::Sender<BroadcastRequest>,
    /// Not joined on drop, for the reason [`SourceOpener`] is not.
    worker: Option<JoinHandle<()>>,
}

impl BroadcastOpener {
    fn spawn(backend: Arc<Backend>, replies: mpsc::Sender<EngineCommand>) -> std::io::Result<Self> {
        let (requests, incoming) = mpsc::channel::<BroadcastRequest>();
        let worker = thread::Builder::new()
            .name("broadcast-opener".to_owned())
            .spawn(move || {
                while let Ok(request) = incoming.recv() {
                    let result = output::connect(&backend, request);
                    // A closed channel means the engine has gone. Whatever
                    // was opened is dropped with the reply, which ends the
                    // connection — there is nothing left to attach it to.
                    if replies
                        .send(EngineCommand::BroadcastOpened(Box::new(result)))
                        .is_err()
                    {
                        break;
                    }
                }
            })?;
        Ok(Self {
            requests,
            worker: Some(worker),
        })
    }

    /// Asks for a connection. A closed channel means the thread is gone,
    /// which the loop learns from the reply that never arrives — it stays in
    /// `Connecting` and the Stop button still works.
    fn request(&self, request: BroadcastRequest) {
        let _ = self.requests.send(request);
    }
}

impl Drop for BroadcastOpener {
    fn drop(&mut self) {
        let (dead, _) = mpsc::channel();
        let _ = std::mem::replace(&mut self.requests, dead);
        drop(self.worker.take());
    }
}

impl Drop for SourceOpener {
    /// Closes the request channel and leaves the thread to finish on its
    /// own.
    ///
    /// Deliberately not joined. What the thread may be inside is unbounded —
    /// a portal picker waits for a user who may never answer — and waiting
    /// for that is the application refusing to quit until they do. Nothing
    /// needs the wait: the thread owns its `Arc<Backend>`, so what it is
    /// opening against cannot be freed under it, and a reply it cannot
    /// deliver is stopped where it lands rather than left running.
    fn drop(&mut self) {
        // Dropped first, or the worker would wait on a channel nothing is
        // going to send down again.
        let (dead, _) = mpsc::channel();
        let _ = std::mem::replace(&mut self.requests, dead);
        drop(self.worker.take());
    }
}

/// What the engine reaches for whatever it is doing: what composites, what
/// the project is told through, and what opens Sources.
///
/// One parameter rather than three, and the reason `apply_command` has room
/// for the arguments that really are its own.
struct Engine<'a> {
    backend: &'a Backend,
    project: Option<&'a ProjectDispatcher>,
    opener: &'a SourceOpener,
    broadcasts: &'a BroadcastOpener,
}

/// Applies one change, reporting whether the running Sources may have moved
/// on — a Scene change can start or stop them, a drag never does.
fn apply_command(
    engine: &Engine<'_>,
    open: &mut HashMap<SceneItemId, SourceState>,
    scene: &mut SourcesSnapshot,
    published: &Published,
    recording: &mut OutputState,
    command: EngineCommand,
) -> bool {
    match command {
        EngineCommand::Scene(snapshot) => {
            *scene = *snapshot;
            reconcile(
                engine,
                recording.mixer_handle(),
                recording.monitor_handle(),
                open,
                scene,
            );
            true
        }
        EngineCommand::Opened(opened) => {
            finish_open(engine, recording.monitor_handle(), open, scene, *opened);
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
                engine.backend.remove_source(&source.name);
            }
            // Left for the next pass rather than opened here, because what
            // asks for a reopen is almost always a settings change and the
            // write behind it has not reached this thread yet — it arrives as
            // the `Scene` snapshot after this command, so opening now would
            // reopen at exactly the settings the user just replaced. Marking
            // it missing hands the open to `reconcile`, which runs on that
            // snapshot; already due, so `retry_missing` still picks it up on
            // its next tick where nothing was changed and no snapshot
            // follows — the Sources dock's reconnect button.
            let item = &scene.items[index];
            let due = Instant::now()
                .checked_sub(retry_after(item))
                .unwrap_or_else(Instant::now);
            open.insert(item_id, SourceState::Missing(due));
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
        EngineCommand::Text(item_id, settings) => {
            if let Some(SourceState::Open(source)) = open.get_mut(&item_id) {
                // Resolved here as everywhere else, so that editing the font
                // of a running clock does not push the format description
                // over the time it is showing.
                push_content(
                    source,
                    PushedContent::Text(source::text::resolved(&settings)),
                );
            }
            false
        }
        EngineCommand::MediaGain(item_id, gain_db) => {
            if let Some(SourceState::Open(source)) = open.get(&item_id) {
                source::set_media_gain_db(source, gain_db);
            }
            false
        }
        EngineCommand::FilterSettings(item_id, filter_id, settings) => {
            if let Some(SourceState::Open(source)) = open.get(&item_id)
                && let Some(filter) = source.filters.iter().find(|open| open.id == filter_id)
            {
                filter.retune(&settings);
            }
            false
        }
        EngineCommand::MediaSeek(item_id, target) => {
            if let Some(SourceState::Open(source)) = open.get(&item_id)
                && let Some(media) = &source.media_file
                && let Err(error) = media
                    .pipeline
                    .seek(target, media_pp::pipeline::SeekMode::Keyframe)
            {
                // Reported and dropped: a refused seek leaves playback where
                // it was, which is a scrub that did nothing rather than a
                // Source that has gone wrong.
                eprintln!("could not seek \"{}\": {error}", source.name);
            }
            false
        }
        EngineCommand::Dragging(item_id, transform, crop) => {
            let Some(index) = scene.items.iter().position(|item| item.id == item_id) else {
                return false;
            };
            let Some(SourceState::Open(source)) = open.get(&item_id) else {
                return false;
            };
            let item = &scene.items[index];
            let layer = layer_for(item, transform, crop, (scene.items.len() - index) as i32);
            let _ = source.layer.set_layer(layer);
            false
        }
        EngineCommand::PreviewVisible(visible) => {
            engine.backend.set_preview_visible(visible);
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
            if recording.running.is_none() && settings.fps != engine.backend.frame_rate() {
                engine.backend.set_frame_rate(settings.fps);
            }
            recording.settings = *settings;
            // The rate the mix runs at decides which audio encoders can open —
            // `libopus` takes 48 kHz and a short list of others, and nothing
            // else. Re-probed here because Apply is when it can have moved.
            recording.audio_codecs = output::available_audio_codecs(recording.mix_format());
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
            match start_recording(engine.backend, recording) {
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
        EngineCommand::StreamingSettings(settings) => {
            recording.streaming = *settings;
            false
        }
        EngineCommand::StartStreaming => {
            // Cleared before the attempt, so what is shown describes this
            // one — the same order the recording's start uses.
            published.streaming_error.store(None);
            request_broadcast(engine, recording, published);
            false
        }
        EngineCommand::BroadcastOpened(opened) => {
            finish_broadcast(engine, recording, published, *opened);
            false
        }
        EngineCommand::StopStreaming => {
            // Asked for by the user, so this also cancels a connect that is
            // still in flight and any retry that was pending — `Off` is what
            // `Broadcast::wanted` reads as "no longer".
            published.streaming_since.store(None);
            published
                .streaming_reconnecting
                .store(false, Ordering::Release);
            match std::mem::replace(&mut recording.broadcast, Broadcast::Off) {
                Broadcast::Live(running) => {
                    if let Err(error) = running.stop(engine.backend) {
                        eprintln!("could not stop streaming cleanly: {error}");
                    }
                }
                Broadcast::Connecting => eprintln!("the broadcast was still connecting"),
                Broadcast::Off | Broadcast::Waiting { .. } => {
                    eprintln!("no broadcast is running");
                }
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
                    if let Err(error) = running.stop(engine.backend) {
                        eprintln!("could not stop recording cleanly: {error}");
                    }
                }
                None => eprintln!("no recording is running"),
            }
            false
        }
    }
}

/// Deliberately not boxed. The lint measures the space every variant costs,
/// which here is a couple of hundred bytes times the number of SceneItems in
/// one Scene — nothing — against an allocation per open Source and a
/// dereference on every reconcile pass, which is the map's whole job.
#[allow(clippy::large_enum_variant)]
enum SourceState {
    Open(OpenSource),
    /// Asked for, and not answered yet.
    ///
    /// Nothing is running behind it and nothing here can hurry it — see
    /// [`SourceOpener`] for why opening happens elsewhere. It is a state
    /// rather than an absence so that the same Source is not asked for twice
    /// while the first attempt is still going, and so what comes back has
    /// somewhere to land.
    Opening,
    /// Opening failed once and will not be retried.
    ///
    /// A retry loop here would reopen the portal dialog on every snapshot,
    /// which is a stream of modal windows rather than an error message.
    Failed,
    /// Opened cleanly, and the thing it captures is not here right now.
    ///
    /// The instant is when it started waiting, because how long to wait is
    /// not one number: a window comes back when the user reopens it and a
    /// camera comes back when it has finished rebooting, so a stream carries
    /// its own interval — see `retry_after`.
    ///
    /// Only a Window Capture reaches this: a window is closed and reopened
    /// as a matter of course, so its absence is a state rather than a
    /// failure. Unlike `Failed` it is looked at again — see `retry_missing`
    /// — and nothing was opened, so there is nothing holding a dialog or a
    /// device while it waits.
    Missing(Instant),
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

/// How long a `Missing` Source waits before it is looked for again, unless
/// its own settings say otherwise.
///
/// The look enumerates every top-level window, so it is not something to do
/// on every idle tick; a second is well under what anyone notices between
/// bringing a window back and seeing it in the Scene.
const MISSING_RETRY: Duration = Duration::from_secs(1);

/// How long this item waits before it is tried again.
///
/// A live stream carries its own, because the wait is a request to somebody
/// else's machine rather than a look at this one: a camera that is rebooting
/// wants to be left alone for a moment, and one on a metered link may not
/// want to be asked at all — which is `None`, and is why such a Source is
/// held `Disconnected` rather than `Missing` in the first place.
fn retry_after(item: &SceneItemSnapshot) -> Duration {
    match &item.settings {
        SourceSettings::Rtsp(settings) => settings.reconnect.unwrap_or(MISSING_RETRY),
        _ => MISSING_RETRY,
    }
}

/// Opens one Scene item, turning both kinds of "no Source" into a state.
fn request_open(
    engine: &Engine<'_>,
    mixer: Option<&media_pp::elements::MixerHandle>,
    open: &mut HashMap<SceneItemId, SourceState>,
    item: &SceneItemSnapshot,
    layer: VideoLayer,
) {
    let request = OpenRequest {
        item: Box::new(item.clone()),
        layer,
        fps: engine.backend.frame_rate(),
        mixer: mixer.cloned(),
    };
    match engine.opener.request(request) {
        // Marked only once the thread has it, so a request that was never
        // taken cannot leave an item waiting for a reply that is not coming.
        Ok(()) => {
            open.insert(item.id, SourceState::Opening);
        }
        Err(error) => {
            eprintln!("could not ask for \"{}\" to be opened: {error}", item.name);
            open.insert(item.id, SourceState::Failed);
        }
    }
}

/// Takes what the opener answered with, if anything is still waiting for it.
///
/// The wait is not held open: a Scene can change, an item can be deleted, and
/// the same Source can be asked for again while the first attempt is still
/// connecting. So what arrives is only installed where the slot still says
/// `Opening` — and a Source with nowhere to go is stopped here, because
/// nothing else is holding it.
fn finish_open(
    engine: &Engine<'_>,
    monitor: Option<media_pp::elements::MixerHandle>,
    open: &mut HashMap<SceneItemId, SourceState>,
    scene: &SourcesSnapshot,
    opened: Opened,
) {
    let id = opened.item.id;
    if !matches!(open.get(&id), Some(SourceState::Opening)) {
        if let Ok(Some(source)) = opened.result {
            source.source.stop();
            engine.backend.remove_source(&source.name);
        }
        return;
    }
    let mut state = state_of(engine.project, &opened.item, opened.result);
    // Placed where the item stands now rather than where it stood when this
    // was asked for: reordering a Scene, or recolouring a Source, while one
    // opens would otherwise take until the next change to show.
    if let SourceState::Open(source) = &mut state
        && let Some((index, item)) = scene
            .items
            .iter()
            .enumerate()
            .find(|(_, item)| item.id == id)
    {
        let _ = source.layer.set_layer(layer_for(
            item,
            item.transform,
            item.crop,
            (scene.items.len() - index) as i32,
        ));
        refresh_pushed(source, item);
        // Here as well as in `reconcile`, so a Source that is monitored is
        // audible from its first buffer rather than from the next pass.
        refresh_media_file(source, item, monitor.as_ref());
    }
    open.insert(id, state);
}

/// Which state one answer from the opener leaves the SceneItem in.
fn state_of(
    project: Option<&ProjectDispatcher>,
    item: &SceneItemSnapshot,
    result: Result<Option<OpenSource>, BackendError>,
) -> SourceState {
    match result {
        // Nothing to open yet, and nothing wrong. Looked at again on the next
        // pass rather than reported.
        Ok(None) => SourceState::Missing(Instant::now()),
        Ok(Some(source)) => {
            // What the Source turned out to be, where that is not what the
            // item was told when it was added. The editor clamps a crop
            // against this, so a stale size is not cosmetic: a crop past the
            // real frame leaves the layer with nothing to draw, and a layer
            // with nothing to draw is simply not drawn.
            // Against what the project stored rather than against the item's
            // shape: an item with no stored size stands in at Canvas size,
            // and a capture that happens to *be* Canvas size would then never
            // record what it is.
            if let (Some(project), Some(size)) = (project, source.negotiated_size)
                && item.settings.size_hint() != Some(size)
            {
                project.dispatch(ProjectCommand::Source(SourceCommand::SetSourceSize(
                    item.id, item.kind, size,
                )));
            }
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

/// Whether opening this item's Source would interrupt whoever is at the
/// screen, so that it must be asked for rather than attempted.
///
/// Only a Window Capture can answer yes, and only where its target is one the
/// portal owns — see `WindowCaptureTarget::can_be_reopened_silently`.
pub(super) fn needs_asking(item: &SceneItemSnapshot) -> bool {
    match &item.settings {
        SourceSettings::WindowCapture(settings) => !settings.target.can_be_reopened_silently(),
        // Not because looking costs a dialog, but because the user said not
        // to: a stream with no reconnect interval is one this may not go back
        // to on its own, and the Sources dock offers it the same way it
        // offers a window whose picker cannot be reopened silently.
        SourceSettings::Rtsp(settings) => settings.reconnect.is_none(),
        _ => false,
    }
}

/// Looks again for whatever a `Missing` Source captures.
///
/// This runs off the idle tick rather than off a Scene change: a window that
/// is closed and reopened while the user does nothing else in the app
/// produces no command at all, so waiting for one would leave the Source
/// blank until something unrelated happened to move.
fn retry_missing(
    engine: &Engine<'_>,
    mixer: Option<&media_pp::elements::MixerHandle>,
    open: &mut HashMap<SceneItemId, SourceState>,
    snapshot: &SourcesSnapshot,
) {
    if !open
        .values()
        .any(|state| matches!(state, SourceState::Missing(_)))
    {
        return;
    }
    let count = snapshot.items.len();
    for (index, item) in snapshot.items.iter().enumerate() {
        let Some(SourceState::Missing(since)) = open.get(&item.id) else {
            continue;
        };
        // Each on its own clock: a stream that asked to be left for a minute
        // must not be reconnected on the tick that suits a window.
        if since.elapsed() < retry_after(item) {
            continue;
        }
        let layer = layer_for(item, item.transform, item.crop, (count - index) as i32);
        request_open(engine, mixer, open, item, layer);
    }
}

/// Brings the running Sources in line with what the project now holds.
/// Applies a Source's filters as the project now has them, and answers
/// whether that needed the Source reopening after all.
///
/// Settings and the enable flag reach the running elements through the
/// handles kept beside them, which is the whole reason those are kept: a
/// slider must not reopen a camera, and on Wayland a reopen is a portal
/// dialog. Adding, removing and reordering used to be a different question,
/// because a chain was only assembled when a Source was opened; they are now
/// a refill of the Source's rack, which the frames flow through the whole
/// time.
///
/// The answer is therefore `true` in two cases only: a Source whose kind has
/// no rack, and one whose refill failed. Building a filter fails on a lost or
/// exhausted device, which is what reopening the Source is the recovery for —
/// so the old path is what a failure falls back to rather than something that
/// no longer exists.
fn refresh_filters(source: &mut OpenSource, item: &SceneItemSnapshot) -> bool {
    if filters::running_shape(&source.filters) == filters::shape(&item.filters) {
        for (open, stored) in source.filters.iter().zip(&item.filters) {
            open.apply(stored);
        }
        return false;
    }

    // Answered before the match so the borrow of the rack ends with it, and
    // the new handles can be written back into the same Source.
    let refilled = match &source.filter_rack {
        Some(rack) => rack.refill(&item.filters),
        None => return true,
    };
    match refilled {
        Ok(filters) => {
            // Built from the stored settings, with the stored enable flag
            // already set, so there is nothing left to apply to them.
            source.filters = filters;
            false
        }
        Err(error) => {
            eprintln!("\"{}\": could not rebuild the filters: {error}", item.name);
            true
        }
    }
}

fn reconcile(
    engine: &Engine<'_>,
    mixer: Option<&media_pp::elements::MixerHandle>,
    monitor: Option<media_pp::elements::MixerHandle>,
    open: &mut HashMap<SceneItemId, SourceState>,
    snapshot: &SourcesSnapshot,
) {
    // Sources whose filter chain is no longer the one they were opened with.
    // Collected rather than reopened here: the arm that notices holds a
    // mutable borrow of `open`, and replacing an entry needs another.
    let mut rebuild: Vec<SceneItemId> = Vec::new();
    let count = snapshot.items.len();
    for (index, item) in snapshot.items.iter().enumerate() {
        // The snapshot is ordered front-most first, and the compositor draws
        // larger z later, so the two run opposite ways.
        let layer = layer_for(item, item.transform, item.crop, (count - index) as i32);
        match open.get_mut(&item.id) {
            Some(SourceState::Open(source)) => {
                let _ = source.layer.set_layer(layer);
                refresh_pushed(source, item);
                refresh_media_file(source, item, monitor.as_ref());
                if refresh_filters(source, item) {
                    rebuild.push(item.id);
                }
            }
            Some(SourceState::Failed | SourceState::Disconnected | SourceState::Ended) => {}
            // Already on its way, and asking again would only open a second
            // one of whatever this is.
            Some(SourceState::Opening) => {}
            Some(SourceState::Missing(_)) | None => {
                request_open(engine, mixer, open, item, layer);
            }
        }
    }

    for item_id in rebuild {
        // Dropping the old one is what `insert` does here, and `Missing` in
        // the past is what makes the next pass open the new chain — the same
        // two steps `EngineCommand::ReopenSource` takes.
        open.insert(item_id, SourceState::Missing(Instant::now()));
    }

    // A Source whose item merely left the Scene is kept, stopped: coming back
    // to that Scene is then a resume rather than another portal round trip,
    // and a stopped capture costs nothing while it waits.
    for (id, state) in open.iter_mut() {
        let SourceState::Open(source) = state else {
            continue;
        };
        let item = snapshot.items.iter().find(|item| item.id == *id);
        let showing = item.is_some();
        // Two questions now, where there used to be one. Leaving the Scene
        // still stops a Source, but a media file can also be paused while its
        // item is right there — so what should be running is both together,
        // and what should be hidden is the Scene alone.
        let running = item.is_some_and(|item| !source::paused(item, showing));
        if running != source.running {
            if running {
                source.source.resume();
            } else {
                source.source.pause();
            }
            source.running = running;
        }
        if showing != source.showing {
            if !showing {
                let _ = source.layer.set_visible(false);
            }
            source.showing = showing;
        }
    }

    // Only an item the project no longer holds anywhere is closed for good.
    open.retain(|id, state| {
        if snapshot.live_items.contains(id) {
            return true;
        }
        if let SourceState::Open(source) = state {
            source.source.stop();
            engine.backend.remove_source(&source.name);
        }
        // An `Opening` entry is dropped with the rest. What arrives for it
        // finds no slot waiting and is stopped where it lands — see
        // `finish_open`.
        false
    });
}

/// Where a SceneItem's layer sits on the Canvas, and in what order.
///
/// The rectangle already carries the Source's own size scaled by the item's
/// Transform, so the fit is [`VideoFit::Stretch`]: whatever aspect the user
/// asked for is expressed in that rectangle, and letterboxing inside it would
/// second-guess them.
fn layer_for(
    item: &SceneItemSnapshot,
    transform: Transform,
    crop: Crop,
    z_index: i32,
) -> VideoLayer {
    let [x, y, width, height] = item.canvas_rect_cropped(transform, crop);
    let mut layer = VideoLayer::new(VideoRect::new(
        x.round() as i32,
        y.round() as i32,
        (width.round() as u32).max(1),
        (height.round() as u32).max(1),
    ));
    layer.z_index = z_index;
    layer.visible = item.visible;
    layer.fit = VideoFit::Stretch;
    layer.source = source_rect(item, crop);
    // NV12 carries no alpha, so a Color Source's own is the layer's opacity
    // rather than something the blend could read out of its pixels.
    if let SourceSettings::Color(settings) = &item.settings {
        layer.opacity = f32::from(settings.rgba[3]) / 255.0;
    }
    layer
}

/// The part of the Source this item shows, or `None` for all of it.
///
/// The item's own crop, in the Source's own pixels — which is the unit it is
/// stored in, and the reason it survives the item being scaled afterwards.
/// `source_size` is a `f32` because the editor works in Canvas units, so the
/// edges are rounded inwards here: a crop that grew by half a pixel would put
/// back a sliver the user had cut off.
///
/// `None` where nothing is cropped, so a layer that was never cropped is the
/// same layer it always was — and where the crop would leave nothing, which
/// the compositor treats as a layer with nothing to draw rather than as an
/// error.
fn source_rect(item: &SceneItemSnapshot, crop: Crop) -> Option<VideoSourceRect> {
    if crop == Crop::default() {
        return None;
    }
    let [source_width, source_height] = item.source_size;
    let x = crop.left.max(0.0).ceil() as u32;
    let y = crop.top.max(0.0).ceil() as u32;
    let width = (source_width - crop.left - crop.right).floor().max(0.0) as u32;
    let height = (source_height - crop.top - crop.bottom).floor().max(0.0) as u32;
    (width > 0 && height > 0).then(|| VideoSourceRect::new(x, y, width, height))
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
    use crate::domain::{SourceKind, WindowCaptureTarget};

    fn window_item(id: i64, target: WindowCaptureTarget) -> SceneItemSnapshot {
        SceneItemSnapshot {
            filters: Vec::new(),
            peak_db: None,
            position: None,
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
            media_meters: Arc::new(ArcSwapOption::empty()),
            encoders: Arc::new(ArcSwapOption::empty()),
            audio_codecs: Arc::new(ArcSwapOption::empty()),
            recording_error: Arc::new(ArcSwapOption::empty()),
            streaming_since: Arc::new(ArcSwapOption::empty()),
            streaming_error: Arc::new(ArcSwapOption::empty()),
            streaming_reconnecting: Arc::new(AtomicBool::new(false)),
            output_load: Arc::new(AtomicU32::new(0)),
            stats: Arc::new(ArcSwapOption::empty()),
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
        colour.settings = SourceSettings::Color(crate::domain::ColorSourceSettings {
            size: [1920.0, 1080.0],
            rgba: [0, 0, 0, 255],
        });
        assert!(
            !needs_asking(&colour),
            "nothing but a Window Capture has a picker behind it"
        );
    }

    fn stream_item(id: i64, reconnect: Option<Duration>) -> SceneItemSnapshot {
        let mut item = window_item(
            id,
            WindowCaptureTarget::Portal {
                restore_token: None,
            },
        );
        item.kind = SourceKind::Rtsp;
        item.settings = SourceSettings::Rtsp(crate::domain::RtspSourceSettings {
            url: "rtsp://10.0.0.7/main".to_owned(),
            transport: crate::domain::RtspTransport::Tcp,
            reconnect,
            size_hint: None,
            has_audio: false,
            gain_db: 0.0,
            muted: false,
        });
        item
    }

    /// A stream that may reconnect waits its own interval; one that may not
    /// is not waiting at all — it is `Disconnected`, and `needs_asking` is
    /// what puts it there.
    #[test]
    fn a_stream_waits_the_interval_it_was_given() {
        let every_minute = stream_item(1, Some(Duration::from_secs(60)));
        assert_eq!(retry_after(&every_minute), Duration::from_secs(60));
        assert!(
            !needs_asking(&every_minute),
            "a stream with an interval reconnects by itself"
        );

        let never = stream_item(2, None);
        assert!(
            needs_asking(&never),
            "a stream told not to reconnect waits to be asked, like a portal window"
        );

        // Everything else is on the tick, which is what a window's search has
        // always run at.
        let window = window_item(
            3,
            WindowCaptureTarget::Window {
                process: "firefox".into(),
                title: "obs-rs".into(),
            },
        );
        assert_eq!(retry_after(&window), MISSING_RETRY);
    }

    /// What the compositor is told to draw, from what the item stores.
    ///
    /// Rounded inwards on every edge, because a crop is in the Source's own
    /// pixels and the editor works in Canvas units: half a pixel back would
    /// put a sliver of what was cut off into the picture.
    #[test]
    fn a_crop_becomes_the_region_the_layer_draws() {
        let mut item = window_item(
            1,
            WindowCaptureTarget::Portal {
                restore_token: None,
            },
        );
        item.source_size = [1920.0, 1080.0];

        assert_eq!(
            source_rect(&item, Crop::default()),
            None,
            "an uncropped layer draws the whole frame, as it always did"
        );

        let region = source_rect(
            &item,
            Crop {
                left: 100.5,
                top: 50.0,
                right: 200.0,
                bottom: 0.0,
            },
        )
        .expect("a crop that leaves something");
        assert_eq!((region.x, region.y), (101, 50));
        assert_eq!((region.width, region.height), (1619, 1030));

        assert_eq!(
            source_rect(
                &item,
                Crop {
                    left: 1920.0,
                    top: 0.0,
                    right: 0.0,
                    bottom: 0.0,
                }
            ),
            None,
            "a crop that leaves nothing is a layer with nothing to draw"
        );
    }

    /// Two claims at once, because the second is what makes the first cheap:
    /// the map says which Sources are dark and why, and it is replaced only
    /// when that answer moves. The UI reads it on every pass.
    #[test]
    fn the_status_map_names_the_dark_sources_and_holds_still() {
        let published = published();
        let mut open = HashMap::new();

        status::publish_source_status(&published, &open);
        assert!(
            published.source_status.load_full().is_none(),
            "nothing has gone wrong yet, so there is nothing to say"
        );

        open.insert(SceneItemId(1), SourceState::Disconnected);
        open.insert(SceneItemId(2), SourceState::Missing(Instant::now()));
        // A file that played out is dark for a different reason, and says so.
        open.insert(SceneItemId(3), SourceState::Ended);
        // Still being opened, which is not a state to report: a badge beside
        // every item for as long as its capture takes to start would say
        // something is wrong on the way to everything working.
        open.insert(SceneItemId(4), SourceState::Opening);
        status::publish_source_status(&published, &open);
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
            ]),
            "an opening Source must not be listed among the dark ones"
        );

        status::publish_source_status(&published, &open);
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
        status::publish_source_status(&published, &open);
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
