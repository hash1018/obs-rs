//! The audio half of the engine: capture, per-source gain, and one mix.
//!
//! Independent of the video half, and deliberately not part of a
//! `engine::backend`. Those three — compositor, capture, wgpu interop — are
//! one unit because what a capture produces decides what can accept it. Audio
//! touches none of that: the only platform-specific line here is which
//! element opens a device, and everything after it is the same on both.
//!
//! It is also independent of whether the video half started. A machine with
//! no usable GPU still has a microphone, and losing the mixer because
//! `Backend::start` failed would be losing it for an unrelated reason.
//!
//! # Shape
//!
//! ```text
//! pipeline "audio-1"  CaptureSource ─ AudioVolume ─ Tee ┬─ AppSink   (meters)
//!                                                       └─┐
//! pipeline "audio-2"  CaptureSource ─ AudioVolume ─ Tee ┬─ AppSink
//!                                                       └─┤
//!                                                         ↓
//! pipeline "audio-mix"                    AudioMixer ─ Tee ─ (a recording, when one runs)
//! ```
//!
//! One pipeline each, not one between them. A `Pipeline`'s sources are fixed
//! when it is built, so anything that has to be reopened has to be alone in
//! one — see [`AudioEngine`] for why the mixer above all.
//!
//! # The meters are post-fader
//!
//! The `Tee` hangs off [`AudioVolume`], not off the capture, so a meter shows
//! what the fader let through rather than what arrived at it — pulling one
//! down empties its meter, and so does muting.
//!
//! A `Tee` is not a `Source` — its outputs live behind a lock instead of in
//! `src_pads` — so nothing can follow it in a chain and it cannot be a `pipe`
//! stage. It is what a chain *ends* at, which is what `ChainBuilder::to_branch`
//! takes: the fan-out and the stages in front of it commit as one subgraph, so
//! the recorded topology puts the branching on the fader that really does it.
//!
//! # What a change costs
//!
//! Gain and mute go through handles and cost nothing. A device change costs
//! that one source's capture, which is reopened and registered with the
//! mixer again — nothing else stops, and the mix keeps its timeline.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::mpsc::{self, Sender};
use std::thread::{self, JoinHandle};

use arc_swap::ArcSwapOption;

use media_pp::{
    buffer::MediaBuffer,
    elements::{
        AppSink, AudioMixer, AudioMixerOptions, AudioVolume, AudioVolumeHandle, MixFormat,
        MixerHandle, TeeBuilder, TeeHandle,
    },
    pipeline::Pipeline,
};

use crate::capture::AudioDeviceTarget;
use crate::domain::{AudioSourceId, AudioSourceKind};
use crate::snapshots::{AudioSnapshot, AudioSourceSnapshot};

use super::backend::BackendError;

/// What every source is mixed into, and therefore what a recording's audio
/// track is made of. 48 kHz stereo is what both backends' devices are
/// overwhelmingly already at, so the mixer's own resampler usually has
/// nothing to do.
/// What the mixer starts at when nothing has been stored yet. What it
/// actually runs at is `AudioSettings`, which it is started with and follows
/// afterwards — ask the `MixerHandle`, never this.
pub(super) const DEFAULT_MIX_FORMAT: MixFormat = MixFormat {
    sample_rate: crate::settings::DEFAULT_SAMPLE_RATE,
    channels: crate::settings::DEFAULT_CHANNELS,
};

/// The quietest a meter shows, matching the mixer dock's own scale.
const METER_FLOOR_DB: f32 = -60.0;

/// One source's most recent peak, in `f32` bits so the UI can read it without
/// a lock.
///
/// Written by whichever thread the capture's own `Tee` pushes on, read by the
/// UI thread. `Relaxed` because a meter that is one frame stale is a meter
/// that is correct a frame later, and nothing else is ordered against it.
/// Cloning shares the counters rather than copying values: the map is a
/// handful of `Arc`s, and the UI reads the same atomics the captures write.
#[derive(Default, Clone)]
pub(super) struct Levels {
    peaks: HashMap<AudioSourceId, Arc<AtomicU32>>,
}

impl Levels {
    /// The peak of the last buffer this source produced, or `None` when it
    /// has produced none — which is what a source that failed to open, or has
    /// not been given a device, looks like.
    pub(super) fn peak_db(&self, id: AudioSourceId) -> Option<f32> {
        let bits = self.peaks.get(&id)?.load(Ordering::Relaxed);
        (bits != 0).then(|| f32::from_bits(bits))
    }

    /// Whether this source has a capture running behind it.
    ///
    /// Its counter exists exactly while it does: `AudioEngine` inserts one
    /// when a source opens and removes it when the source closes, so asking
    /// whether the counter is here is asking whether the capture is.
    pub(super) fn is_running(&self, id: AudioSourceId) -> bool {
        self.peaks.contains_key(&id)
    }
}

/// One source that is open, in its own pipeline, and how it was opened.
struct OpenAudioSource {
    /// This source's capture, tee and fader. Dropping it stops that one
    /// capture and nothing else.
    _pipeline: Arc<Pipeline>,
    /// What it is registered with the mixer as, which is how it is
    /// deregistered again.
    name: String,
    volume: AudioVolumeHandle,
    /// What this was opened with, so a snapshot naming something else is
    /// recognised as needing a reopen rather than a handle call.
    device: Option<String>,
}

/// The mixer, and the sources feeding it.
///
/// # The mixer outlives its inputs
///
/// It is built once and never rebuilt, in a pipeline of its own that holds
/// nothing else. Its output timestamps are a count from when it started, so
/// restarting it restarts that count — which is invisible today, when nothing
/// consumes the mix, and would be a recording losing its audio timeline the
/// moment one does.
///
/// So a device change closes and reopens *that source's* pipeline and
/// re-registers it with the mixer through [`MixerHandle`], which is a runtime
/// registration for exactly this. The mixer never notices, and neither does
/// the source beside it: changing the microphone used to restart desktop
/// audio too, because both lived in the one pipeline a change had to rebuild.
pub(super) struct AudioEngine {
    /// `None` only if building it failed, which leaves the sources with
    /// nowhere to mix into and is reported once.
    mixer: Option<RunningMixer>,
    sources: HashMap<AudioSourceId, OpenAudioSource>,
    levels: Levels,
}

struct RunningMixer {
    _pipeline: Arc<Pipeline>,
    handle: MixerHandle,
    /// Where a recording's audio track attaches, and the reason the mixer
    /// fans out at all — see [`start_mixer`].
    tee: TeeHandle,
}

impl AudioEngine {
    /// Starts the mixer. It runs from here until this is dropped, whether or
    /// not anything is feeding it.
    pub(super) fn new(format: MixFormat) -> Self {
        let mixer = match start_mixer(format) {
            Ok(mixer) => Some(mixer),
            Err(error) => {
                eprintln!("could not start the audio mixer: {error}");
                None
            }
        };
        Self {
            mixer,
            sources: HashMap::new(),
            levels: Levels::default(),
        }
    }

    pub(super) fn levels(&self) -> &Levels {
        &self.levels
    }

    /// Changes what the mix is summed into, from the mixer's next tick.
    ///
    /// A handle call, not a restart: the mixer keeps its own sample count and
    /// so its timeline, which is the reason it is alone in a pipeline of its
    /// own to begin with. Every input notices and rebuilds its own resampler.
    pub(super) fn set_mix_format(&self, format: MixFormat) {
        let Some(mixer) = &self.mixer else {
            return;
        };
        if mixer.handle.mix_format() == Some(format) {
            return;
        }
        if !mixer.handle.set_mix_format(format) {
            eprintln!(
                "the audio mixer refused {}Hz, {} channel(s)",
                format.sample_rate, format.channels
            );
        }
    }

    /// Where a recording's audio track attaches, or `None` when the mixer
    /// never started — in which case a recording is written without one
    /// rather than refused.
    pub(super) fn mixer_access(&self) -> Option<(TeeHandle, MixerHandle)> {
        self.mixer
            .as_ref()
            .map(|mixer| (mixer.tee.clone(), mixer.handle.clone()))
    }

    /// Brings the running sources in line with what the project holds and
    /// what the machine currently has plugged in.
    ///
    /// Gain and mute are handle calls on a source that is already open. A
    /// device change, or a source appearing or going, touches only that
    /// source. Called again on every endpoint change too, which is what
    /// opens a source whose device has just arrived — and closes one whose
    /// device has just left.
    /// Sets one open source's gain, for a fader being dragged.
    ///
    /// Nothing to do for a source with no capture behind it: there is no
    /// handle to set, and the next `apply` carries the value once the project
    /// has it.
    pub(super) fn set_gain_db(&mut self, id: AudioSourceId, gain_db: f32) {
        if let Some(open) = self.sources.get(&id) {
            let _ = open.volume.set_gain_db(gain_db);
        }
    }

    pub(super) fn apply(&mut self, snapshot: &AudioSnapshot, devices: &[AudioDeviceTarget]) {
        if self.mixer.is_none() {
            return;
        }

        // Gone from the project: close it and take its registration back.
        let wanted: Vec<AudioSourceId> = snapshot.items.iter().map(|source| source.id).collect();
        let dropped: Vec<AudioSourceId> = self
            .sources
            .keys()
            .copied()
            .filter(|id| !wanted.contains(id))
            .collect();
        for id in dropped {
            self.close(id);
        }

        for source in &snapshot.items {
            // Nothing to open it on. Closing rather than leaving it running
            // is what an unplugged microphone needs: the capture behind it
            // has lost its endpoint, and the mixer dock hides a source that
            // is not running — so one left in the map would show a channel
            // with a meter that can never move again.
            if !device_available(devices, source) {
                self.close(source.id);
                continue;
            }
            match self.sources.get(&source.id) {
                // Already open on the endpoint asked for: the fader and the
                // mute button are all that can have changed.
                Some(open) if open.device == source.device => {
                    let _ = open.volume.set_gain_db(source.gain_db);
                    open.volume.set_muted(source.muted);
                }
                _ => self.reopen(source),
            }
        }
    }

    /// Stops one source and takes its mixer registration back, leaving the
    /// project's own entry alone — this is about what is running, not about
    /// what the user asked for.
    ///
    /// Removing its counter is what tells the dock the channel is gone, so
    /// every path that stops a source goes through here rather than dropping
    /// it out of the map directly.
    fn close(&mut self, id: AudioSourceId) {
        let Some(mixer) = &self.mixer else {
            return;
        };
        if let Some(open) = self.sources.remove(&id) {
            mixer.handle.remove_source(&open.name);
        }
        self.levels.peaks.remove(&id);
    }

    /// Closes this source if it was open and opens it again on the endpoint
    /// the project now names.
    fn reopen(&mut self, source: &AudioSourceSnapshot) {
        let name = format!("audio-{}", source.id.0);
        // Closed before the new one opens: two captures of one endpoint is
        // something both backends allow and neither is what was asked for.
        self.close(source.id);
        let Some(mixer) = &self.mixer else {
            return;
        };

        match open_source(&mixer.handle, &name, source) {
            Ok((open, peak)) => {
                self.levels.peaks.insert(source.id, peak);
                self.sources.insert(source.id, open);
            }
            Err(error) => {
                // One source that cannot open must not cost the others
                // theirs — a missing microphone is not a reason to lose
                // desktop audio. The mixer dock leaves its channel out until
                // it opens, which is what says so.
                eprintln!("could not open audio source {}: {error}", source.name);
                mixer.handle.remove_source(&name);
            }
        }
    }
}

/// Whether opening this source could find an endpoint at all.
///
/// The same question [`pick`] answers, asked of a list instead of by opening
/// one: the endpoint it stored if that is still there, and otherwise the
/// default for its kind, which is what `pick` falls back to. The two have to
/// agree — this deciding a source is unopenable that `pick` would have opened
/// is a channel missing from the dock for no reason.
fn device_available(devices: &[AudioDeviceTarget], source: &AudioSourceSnapshot) -> bool {
    devices.iter().any(|device| {
        device.kind == source.kind
            && (source.device.as_deref() == Some(device.id.as_str()) || device.is_default)
    })
}

/// The mixer, alone in a pipeline of its own, fanning out through a `Tee`.
///
/// Alone because it must outlive every source that comes and goes through it
/// — see [`AudioEngine`]. It emits whether or not anything is listening,
/// which is what a recording attached later needs.
///
/// The `Tee` is built with no branches on it at all, and `build_dynamic` so
/// that one can be added while it runs: a recording's audio track is opened
/// long after this, and the mixer must not be rebuilt to take it. It is the
/// same arrangement the compositor's own output `Tee` has, for the same
/// reason.
fn start_mixer(format: MixFormat) -> Result<RunningMixer, BackendError> {
    let (mixer, handle) = AudioMixer::new(
        "audio-mixer",
        AudioMixerOptions {
            sample_rate: format.sample_rate,
            channels: format.channels,
        },
    );
    let mut tee = None;
    let pipeline = Pipeline::new("audio-mix", mixer, |source, context| {
        let (tee_branch, tee_handle) =
            TeeBuilder::new("mix-tee", context.clone()).build_dynamic()?;
        context.attach(source, 0, tee_branch)?;
        tee = Some(tee_handle);
        Ok(())
    })?;
    let tee = tee.expect("Pipeline::new runs the builder before returning");
    pipeline.run()?;
    Ok(RunningMixer {
        _pipeline: pipeline,
        handle,
        tee,
    })
}

/// Opens one capture and wires it to the mixer, in a pipeline of its own.
fn open_source(
    mixer: &MixerHandle,
    name: &str,
    source: &AudioSourceSnapshot,
) -> Result<(OpenAudioSource, Arc<AtomicU32>), BackendError> {
    let mixer_input = mixer.add_source(name).ok_or("the audio mixer is gone")?;
    let capture = open_capture(name, source.kind, source.device.as_deref())?;

    let (volume, volume_handle) = AudioVolume::new(format!("{name}-volume"));
    let _ = volume_handle.set_gain_db(source.gain_db);
    volume_handle.set_muted(source.muted);

    let peak = Arc::new(AtomicU32::new(0));
    let meter = AppSink::new(format!("{name}-meter"), {
        let peak = Arc::clone(&peak);
        move |buffer| {
            if let MediaBuffer::Audio(frame) = &buffer {
                peak.store(peak_db(frame).to_bits(), Ordering::Relaxed);
            }
            Ok(())
        }
    });

    let tee_name = format!("{name}-tee");
    let pipeline = Pipeline::new(name, capture, move |source_element, context| {
        // The `Tee` hangs off the *fader*, not the capture, so what both
        // branches carry is what the fader let through — a meter that
        // measures the level rather than the one before it.
        let meter_branch = context.branch().to(Box::new(meter))?;
        let mix_branch = context.branch().to(mixer_input)?;
        let tee = TeeBuilder::new(tee_name, context.clone())
            .branch(meter_branch)
            .branch(mix_branch)
            .build()?;
        // `to_branch` rather than `to`, because a `Tee` is a finished branch
        // rather than a `Sink` — it is what a chain ends *at*. Attaching it
        // to the fader's pad on its own would link the same buffers but
        // record the fan-out as the capture's; see `ChainBuilder::to_branch`.
        let faded = context.branch().pipe(volume).to_branch(tee)?;
        context.attach(source_element, 0, faded)?;
        Ok(())
    })?;
    pipeline.run()?;

    Ok((
        OpenAudioSource {
            _pipeline: pipeline,
            name: name.to_owned(),
            volume: volume_handle,
            device: source.device.clone(),
        },
        peak,
    ))
}

/// The loudest sample in this buffer, in decibels below full scale, floored
/// where the mixer's scale ends.
///
/// Peak rather than RMS: a meter is watched to catch a clip, and an average
/// is exactly what hides one.
fn peak_db(frame: &media_pp::ffmpeg::frame::Audio) -> f32 {
    use media_pp::ffmpeg::format::Sample;

    let peak = match frame.format() {
        Sample::F32(_) => frame
            .plane::<f32>(0)
            .iter()
            .fold(0.0f32, |loudest, sample| loudest.max(sample.abs())),
        Sample::I16(_) => frame
            .plane::<i16>(0)
            .iter()
            .fold(0.0f32, |loudest, sample| {
                loudest.max(f32::from(*sample).abs() / f32::from(i16::MAX))
            }),
        // Anything else is not read rather than read wrongly: a meter that
        // shows a plausible number for a format it guessed at is worse than
        // one that shows nothing.
        _ => return METER_FLOOR_DB,
    };
    if peak <= 0.0 {
        return METER_FLOOR_DB;
    }
    (20.0 * peak.log10()).clamp(METER_FLOOR_DB, 0.0)
}

/// Opens the endpoint a source names, or the system default when it names
/// none.
#[cfg(target_os = "windows")]
fn open_capture(
    name: &str,
    kind: AudioSourceKind,
    device: Option<&str>,
) -> Result<media_pp::elements::WasapiCaptureSource, BackendError> {
    use media_pp::elements::{
        WasapiCaptureOptions, WasapiCaptureSource, WasapiDevice, WasapiDeviceKind,
    };

    let wanted = match kind {
        AudioSourceKind::Output => WasapiDeviceKind::Render,
        AudioSourceKind::Input => WasapiDeviceKind::Capture,
    };
    let devices = WasapiCaptureSource::list_devices()?;
    let device: WasapiDevice = pick(
        devices,
        device,
        |device| &device.id,
        |device| device.kind == wanted && device.is_default,
    )?;
    Ok(WasapiCaptureSource::open(name, WasapiCaptureOptions { device })?.0)
}

#[cfg(target_os = "linux")]
fn open_capture(
    name: &str,
    kind: AudioSourceKind,
    device: Option<&str>,
) -> Result<media_pp::elements::PipeWireAudioCaptureSource, BackendError> {
    use media_pp::elements::{
        PipeWireAudioCaptureOptions, PipeWireAudioCaptureSource, PipeWireAudioDevice,
        PipeWireAudioDeviceKind,
    };

    let wanted = match kind {
        AudioSourceKind::Output => PipeWireAudioDeviceKind::Sink,
        AudioSourceKind::Input => PipeWireAudioDeviceKind::Source,
    };
    let devices = PipeWireAudioCaptureSource::list_devices()?;
    // The node *name*, not the id: an id is valid only while its node is, so
    // a stored one would stop resolving after a replug. See
    // `capture::AudioDeviceTarget::id`.
    let device: PipeWireAudioDevice = pick(
        devices,
        device,
        |device| &device.name,
        |device| device.kind == wanted && device.is_default,
    )?;
    Ok(PipeWireAudioCaptureSource::open(name, PipeWireAudioCaptureOptions { device })?.0)
}

#[cfg(not(any(target_os = "windows", target_os = "linux")))]
fn open_capture(
    _name: &str,
    _kind: AudioSourceKind,
    _device: Option<&str>,
) -> Result<media_pp::elements::TestAudioSource, BackendError> {
    Err("no audio capture is written for this platform yet".into())
}

/// The stored endpoint if it is still there, otherwise the system default for
/// this kind.
///
/// Falling back rather than failing: a device that was unplugged should leave
/// the source working on whatever replaced it, which is what somebody who
/// never opened the picker would already have.
#[cfg(any(target_os = "windows", target_os = "linux"))]
fn pick<T>(
    devices: Vec<T>,
    stored: Option<&str>,
    identity: impl Fn(&T) -> &String,
    is_default: impl Fn(&T) -> bool,
) -> Result<T, BackendError> {
    if let Some(stored) = stored
        && let Some(found) = devices.iter().position(|device| identity(device) == stored)
    {
        let mut devices = devices;
        return Ok(devices.swap_remove(found));
    }
    devices
        .into_iter()
        .find(is_default)
        .ok_or_else(|| "this machine reports no default audio device of that kind".into())
}

/// What reaches the audio thread.
///
/// Two things move the graph, and they are not the same event: the project
/// saying what should be running, and the machine saying what it can be run
/// on. Either alone is not enough to decide anything — a source is open when
/// the project asks for it *and* an endpoint exists to open it on.
enum AudioCommand {
    /// What the project now holds.
    Project(Box<AudioSnapshot>),
    /// One source's gain, mid-gesture.
    ///
    /// Separate from `Project` because a fader being dragged is not an edit
    /// yet: the project hears about it once, when the gesture ends, and this
    /// is what the audio hears in the meantime. Same split the Preview's own
    /// drag makes between the compositor and the project.
    Gain(AudioSourceId, f32),
    /// An endpoint appeared, went, or became the default. Says to look
    /// again, not what changed — see [`crate::capture::watch_audio_devices`].
    DevicesChanged,
    /// What the mix should be summed into from now on.
    MixFormat(MixFormat),
    /// Ends the worker's loop.
    ///
    /// Every sender dropping cannot do it: the endpoint watch owns a sender
    /// of its own, and the watch lives on the worker's own stack — so the
    /// channel stays open exactly as long as the loop that would close it,
    /// and `recv` blocks forever. Being told is the way out.
    Shutdown,
}

/// Owns the audio graph on a thread of its own.
///
/// Separate from `EngineManager` rather than folded into it: building a
/// capture opens the audio subsystem and can block for a moment, which is not
/// something to do on the UI thread — and the video engine's thread exits
/// when its backend cannot start, which is no reason to lose the microphone.
pub struct AudioManager {
    /// `Option` only so `Drop` can close the channel by taking it, which is
    /// what ends the worker's loop.
    commands: Option<Sender<AudioCommand>>,
    /// Republished on every rebuild, because the set of sources it covers
    /// changes with them. `None` until the first graph is built.
    levels: Arc<ArcSwapOption<Levels>>,
    /// Every endpoint the machine currently has, for the device pickers.
    ///
    /// Published from here rather than enumerated by the UI because this is
    /// the side that is told when the answer changes. A list taken once at
    /// startup is missing whatever was plugged in since.
    devices: Arc<ArcSwapOption<Vec<AudioDeviceTarget>>>,
    /// Where a recording attaches its audio track, taken once here rather
    /// than asked for later: the recording is opened on the *video* thread,
    /// which cannot reach into this one to fetch it. `None` when the mixer
    /// failed to start.
    /// The mixer's fan-out and its control, taken once at startup because the
    /// mixer lives on a thread the engine cannot ask. `None` when it never
    /// started, which records video only.
    mixer: Option<(TeeHandle, MixerHandle)>,
    worker: Option<JoinHandle<()>>,
}

impl AudioManager {
    pub fn spawn(format: MixFormat, wake_ui: impl Fn() + Send + 'static) -> std::io::Result<Self> {
        let (commands, command_rx) = mpsc::channel::<AudioCommand>();
        let levels = Arc::new(ArcSwapOption::empty());
        let devices = Arc::new(ArcSwapOption::empty());
        // The engine is built on its own thread and stays there — it holds
        // FFmpeg state that is not `Send`, so it cannot be made here and
        // moved. Only the mixer's `Tee` comes back, over this channel: a
        // recording is opened on the *video* thread, which has no way to
        // reach into this one and ask for it later.
        let (ready, mixer_rx) = mpsc::channel::<Option<(TeeHandle, MixerHandle)>>();
        let worker = thread::Builder::new().name("audio".to_owned()).spawn({
            let levels = Arc::clone(&levels);
            let published_devices = Arc::clone(&devices);
            let watch_commands = commands.clone();
            move || {
                let mut engine = AudioEngine::new(format);
                let _ = ready.send(engine.mixer_access());
                let mut known_devices = crate::capture::audio_devices();
                published_devices.store(Some(Arc::new(known_devices.clone())));
                // Held for the life of the loop: dropping it stops the
                // notifications, and the callback it owns is what sends the
                // command below.
                let _watch = crate::capture::watch_audio_devices(move || {
                    let _ = watch_commands.send(AudioCommand::DevicesChanged);
                });
                // The project's own snapshot, kept because an endpoint
                // change has to be answered with it — what to run is still
                // the project's answer, and the machine only changed what
                // can be run.
                let mut project: Option<AudioSnapshot> = None;

                // Ends when the manager says so — see `AudioCommand::Shutdown`.
                // Dropping the engine on the way out stops every capture and
                // the mixer, and dropping the watch stops the notifications.
                while let Ok(first) = command_rx.recv() {
                    // Coalesced before anything is acted on. Windows raises
                    // several notifications for one plug, and a fader being
                    // dragged sends a snapshot per frame; both collapse to
                    // the one state they all describe.
                    let mut devices_changed = false;
                    let mut project_changed = false;
                    let mut ending = false;
                    let mut gains: Vec<(AudioSourceId, f32)> = Vec::new();
                    for command in std::iter::once(first).chain(command_rx.try_iter()) {
                        match command {
                            AudioCommand::Project(snapshot) => {
                                project = Some(*snapshot);
                                project_changed = true;
                            }
                            AudioCommand::Gain(id, gain_db) => {
                                // Last one wins per source: a drag sends one
                                // per frame and only the newest is where the
                                // fader is now.
                                gains.retain(|(other, _)| *other != id);
                                gains.push((id, gain_db));
                            }
                            AudioCommand::DevicesChanged => devices_changed = true,
                            AudioCommand::MixFormat(format) => {
                                engine.set_mix_format(format);
                            }
                            AudioCommand::Shutdown => ending = true,
                        }
                    }
                    if ending {
                        break;
                    }
                    if devices_changed {
                        known_devices = crate::capture::audio_devices();
                        published_devices.store(Some(Arc::new(known_devices.clone())));
                    }
                    // Only when something it reads has moved. A fader being
                    // dragged wakes this loop sixty times a second, and
                    // reconciling the whole graph against a project that has
                    // not changed is the work `AudioCommand::Gain` exists to
                    // avoid — it would also re-apply the stored gain over the
                    // one being dragged, every frame.
                    if (project_changed || devices_changed)
                        && let Some(project) = &project
                    {
                        engine.apply(project, &known_devices);
                    }
                    // After any apply, not before: a project that arrived in
                    // the same batch carries the gain from before the drag,
                    // and applying it second would undo every frame of one.
                    for (id, gain_db) in gains {
                        engine.set_gain_db(id, gain_db);
                    }
                    // Cloned, not taken: an apply that changed nothing must
                    // leave the engine holding the counters it is still
                    // writing into.
                    levels.store(Some(Arc::new(engine.levels().clone())));
                    wake_ui();
                }
            }
        })?;
        Ok(Self {
            commands: Some(commands),
            levels,
            devices,
            // Waits only for the mixer to be built, which is the worker's
            // first act. An `Err` means it never got that far, which is the
            // same answer as a mixer that failed: record without audio.
            mixer: mixer_rx.recv().ok().flatten(),
            worker: Some(worker),
        })
    }

    /// Sets one source's gain now, without waiting for the project.
    ///
    /// For a fader being dragged. What is heard follows the handle, and the
    /// project is told when the gesture ends — so the level moves under the
    /// pointer while the database still records one edit per drag.
    ///
    /// Dropped if the source is not open. A gain arriving for something with
    /// no capture behind it has nothing to set, and the next `apply` carries
    /// the value anyway.
    pub fn set_gain_db(&self, id: AudioSourceId, gain_db: f32) {
        if let Some(commands) = &self.commands {
            let _ = commands.send(AudioCommand::Gain(id, gain_db));
        }
    }

    /// Tells the audio graph what the project now holds.
    pub fn apply(&self, snapshot: &AudioSnapshot) {
        if let Some(commands) = &self.commands {
            let _ = commands.send(AudioCommand::Project(Box::new(snapshot.clone())));
        }
    }

    /// The most recent peak for one source, or `None` when nothing is
    /// measuring it.
    pub fn peak_db(&self, id: AudioSourceId) -> Option<f32> {
        self.levels.load_full()?.peak_db(id)
    }

    /// Whether this source has a capture running behind it.
    ///
    /// `None` while nothing has been published yet — not "no", because the
    /// two mean opposite things to a dock deciding what to draw, and the
    /// first pass would otherwise hide every channel for a frame.
    pub fn is_running(&self, id: AudioSourceId) -> Option<bool> {
        Some(self.levels.load_full()?.is_running(id))
    }

    /// Every audio endpoint the machine currently has, or `None` before the
    /// first enumeration.
    /// Where a recording's audio track attaches, or `None` when the mixer
    /// never started.
    /// Where a recording's audio track attaches, and the control that says
    /// what format it will be in.
    pub fn mixer(&self) -> Option<(TeeHandle, MixerHandle)> {
        self.mixer.clone()
    }

    /// Tells the mixer what to sum into from now on.
    pub fn set_mix_format(&self, format: MixFormat) {
        if let Some(commands) = &self.commands {
            let _ = commands.send(AudioCommand::MixFormat(format));
        }
    }

    pub fn devices(&self) -> Option<Arc<Vec<AudioDeviceTarget>>> {
        self.devices.load_full()
    }
}

impl Drop for AudioManager {
    fn drop(&mut self) {
        // Told, not merely let go of. The worker owns the endpoint watch,
        // which owns a sender for this same channel — so dropping this one
        // leaves `recv` waiting on a sender only the worker could release,
        // and the join below would never return.
        if let Some(commands) = self.commands.take() {
            let _ = commands.send(AudioCommand::Shutdown);
        }
        // Joined rather than detached, because dropping the engine it owns
        // is what stops the captures and the mixer.
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    fn source(kind: AudioSourceKind, device: Option<&str>) -> AudioSourceSnapshot {
        AudioSourceSnapshot {
            id: AudioSourceId(1),
            name: "test".to_owned(),
            kind,
            device: device.map(str::to_owned),
            gain_db: 0.0,
            muted: false,
            peak_db: None,
            running: true,
        }
    }

    fn device(id: &str, kind: AudioSourceKind, is_default: bool) -> AudioDeviceTarget {
        AudioDeviceTarget {
            id: id.to_owned(),
            name: id.to_owned(),
            kind,
            is_default,
        }
    }

    /// A source that named nothing follows the default, so it is openable
    /// exactly while one of its kind exists.
    #[test]
    fn a_source_with_no_device_needs_a_default_of_its_kind() {
        let mic = device("mic", AudioSourceKind::Input, true);
        let speakers = device("speakers", AudioSourceKind::Output, true);

        assert!(device_available(
            &[mic.clone(), speakers.clone()],
            &source(AudioSourceKind::Input, None)
        ));
        // Only playback endpoints: an input source has nothing to open.
        assert!(!device_available(
            &[speakers],
            &source(AudioSourceKind::Input, None)
        ));
        assert!(!device_available(
            &[],
            &source(AudioSourceKind::Input, None)
        ));
    }

    /// The stored endpoint counts whether or not it is the default one —
    /// that is the whole point of having stored it.
    #[test]
    fn a_stored_device_counts_without_being_the_default() {
        let devices = [
            device("built-in", AudioSourceKind::Input, true),
            device("usb", AudioSourceKind::Input, false),
        ];
        assert!(device_available(
            &devices,
            &source(AudioSourceKind::Input, Some("usb"))
        ));
    }

    /// `pick` falls back to the default when the stored endpoint is gone, so
    /// this has to call that source openable. The two disagreeing is a
    /// channel missing from the dock that would have opened fine.
    #[test]
    fn a_stored_device_that_is_gone_falls_back_like_pick_does() {
        let devices = [device("built-in", AudioSourceKind::Input, true)];
        assert!(device_available(
            &devices,
            &source(AudioSourceKind::Input, Some("unplugged"))
        ));

        // ...and with nothing of that kind left, neither of them can.
        let devices = [device("speakers", AudioSourceKind::Output, true)];
        assert!(!device_available(
            &devices,
            &source(AudioSourceKind::Input, Some("unplugged"))
        ));
    }

    /// A machine can report endpoints while calling none of them default.
    /// `pick` fails there unless the stored one matches, and this has to say
    /// the same.
    #[test]
    fn endpoints_with_no_default_are_only_openable_by_name() {
        let devices = [device("usb", AudioSourceKind::Input, false)];
        assert!(device_available(
            &devices,
            &source(AudioSourceKind::Input, Some("usb"))
        ));
        assert!(!device_available(
            &devices,
            &source(AudioSourceKind::Input, None)
        ));
    }

    /// Dropping the manager has to stop its worker and return.
    ///
    /// It did not, once: the endpoint watch holds a `Sender` for the same
    /// channel the worker is receiving on, and the watch lives on the
    /// worker's own stack — so dropping the manager's sender left one alive
    /// that only the worker could release, `recv` never returned, and the
    /// `join` in `Drop` hung the application on exit.
    ///
    /// Run on a thread of its own with a deadline rather than inline,
    /// because the failure being caught is a hang: inline, this test would
    /// not fail, it would stop.
    #[test]
    fn dropping_the_manager_returns_rather_than_hanging() {
        let (finished, done) = mpsc::channel();
        thread::Builder::new()
            .name("audio-drop-under-test".to_owned())
            .spawn(move || {
                // The mixer starts either way; captures do not, since no
                // project snapshot is ever sent. That is enough — the
                // channel and the watch are what this is about.
                let manager = AudioManager::spawn(DEFAULT_MIX_FORMAT, || {});
                drop(manager);
                let _ = finished.send(());
            })
            .expect("spawning the thread under test");

        assert!(
            done.recv_timeout(Duration::from_secs(10)).is_ok(),
            "dropping AudioManager did not return: its worker is still waiting on a sender \
             nothing will drop"
        );
    }
}
