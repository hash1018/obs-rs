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
//! CaptureSource ─ Tee ┬─ AppSink                   (peak, for the meters)
//!                     └─ AudioVolume ─ AudioMixer input
//! CaptureSource ─ Tee ┬─ AppSink
//!                     └─ AudioVolume ─ AudioMixer input
//!                                      AudioMixer ─ (recording, later)
//! ```
//!
//! # The meters are pre-fader
//!
//! The tap is the `Tee`'s own branch, ahead of [`AudioVolume`], so a meter
//! shows what the device is producing rather than what the fader is letting
//! through. That is a normal choice — it is what a console's default metering
//! is — and here it is also the only one a `ChainBuilder` expresses: a chain
//! ends at a `Sink`, and a `Tee` is attached to a source rather than reached
//! through filters, so nothing can sit between the two.
//!
//! Muting still empties the meter, because the mixer dock draws a muted
//! channel as silent whatever its peak says. Pulling a fader down is what
//! this cannot show, and a fader is a thing somebody is looking at while they
//! move it.
//!
//! # What a change costs
//!
//! Gain and mute go through handles and cost nothing. A device change rebuilds
//! the whole graph, because a `Pipeline`'s sources are fixed when it is built
//! and a capture element is bound to the endpoint it opened. That is rare —
//! it happens when somebody picks from the menu — and the alternative is
//! machinery for a case that occurs once.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::mpsc::{self, Sender};
use std::thread::{self, JoinHandle};

use arc_swap::ArcSwapOption;

use media_pp::{
    buffer::MediaBuffer,
    elements::{
        AppSink, AudioMixer, AudioMixerOptions, AudioVolume, AudioVolumeHandle, TeeBuilder,
    },
    pipeline::Pipeline,
};

use crate::domain::{AudioSourceId, AudioSourceKind};
use crate::snapshots::AudioSnapshot;

use super::backend::BackendError;

/// What every source is mixed into, and therefore what a recording's audio
/// track is made of. 48 kHz stereo is what both backends' devices are
/// overwhelmingly already at, so the mixer's own resampler usually has
/// nothing to do.
const MIX_SAMPLE_RATE: u32 = 48_000;
const MIX_CHANNELS: u16 = 2;

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
}

/// One source that is open, and how it was opened.
struct OpenAudioSource {
    volume: AudioVolumeHandle,
    /// What this was opened with, so a snapshot that names something else is
    /// recognised as needing a rebuild rather than a handle update.
    device: Option<String>,
}

/// Every audio source running, the mix they feed, and the pipeline that owns
/// them.
pub(super) struct AudioEngine {
    /// `None` until something is open. Dropping it stops every capture and
    /// the mixer with them.
    running: Option<Running>,
    levels: Levels,
}

struct Running {
    _pipeline: Arc<Pipeline>,
    sources: HashMap<AudioSourceId, OpenAudioSource>,
}

impl AudioEngine {
    pub(super) fn new() -> Self {
        Self {
            running: None,
            levels: Levels::default(),
        }
    }

    pub(super) fn levels(&self) -> &Levels {
        &self.levels
    }

    /// Brings the running graph in line with what the project holds.
    ///
    /// Gain and mute are handle calls. Anything else — a source appearing, a
    /// device changing — rebuilds, because a `Pipeline`'s sources are settled
    /// when it is built.
    pub(super) fn apply(&mut self, snapshot: &AudioSnapshot) {
        if self.matches(snapshot) {
            self.update_levels(snapshot);
            return;
        }
        // Dropped before the new one is built: two captures of the same
        // endpoint is a thing WASAPI allows and PipeWire allows, and neither
        // is what was asked for.
        self.running = None;
        match build(snapshot, &mut self.levels) {
            Ok(running) => self.running = Some(running),
            Err(error) => eprintln!("could not start audio: {error}"),
        }
        self.update_levels(snapshot);
    }

    /// Whether the running graph is of the same sources, on the same devices.
    fn matches(&self, snapshot: &AudioSnapshot) -> bool {
        let Some(running) = &self.running else {
            return snapshot.items.is_empty();
        };
        running.sources.len() == snapshot.items.len()
            && snapshot.items.iter().all(|source| {
                running
                    .sources
                    .get(&source.id)
                    .is_some_and(|open| open.device == source.device)
            })
    }

    /// Pushes the faders and mute buttons through, which needs no rebuild.
    fn update_levels(&mut self, snapshot: &AudioSnapshot) {
        let Some(running) = &self.running else {
            return;
        };
        for source in &snapshot.items {
            let Some(open) = running.sources.get(&source.id) else {
                continue;
            };
            let _ = open.volume.set_gain_db(source.gain_db);
            open.volume.set_muted(source.muted);
        }
    }
}

/// Builds one pipeline holding every source and the mixer they feed.
///
/// One pipeline rather than one per source: they share a clock and a bus, and
/// the mixer is only meaningful alongside the inputs registered with it.
fn build(snapshot: &AudioSnapshot, levels: &mut Levels) -> Result<Running, BackendError> {
    levels.peaks.clear();
    if snapshot.items.is_empty() {
        return Err("no audio sources".into());
    }

    let (mixer, mixer_handle) = AudioMixer::new(
        "audio-mixer",
        AudioMixerOptions {
            sample_rate: MIX_SAMPLE_RATE,
            channels: MIX_CHANNELS,
        },
    );

    let mut builder = media_pp::pipeline::PipelineBuilder::new("audio");
    let mut sources = HashMap::new();

    for source in &snapshot.items {
        let name = format!("audio-{}", source.id.0);
        let mixer_input = mixer_handle
            .add_source(&name)
            .ok_or("the audio mixer is gone")?;
        let peak = Arc::new(AtomicU32::new(0));
        levels.peaks.insert(source.id, Arc::clone(&peak));

        let capture = match open_capture(&name, source.kind, source.device.as_deref()) {
            Ok(capture) => capture,
            Err(error) => {
                // One source that cannot open must not cost the others theirs
                // — a missing microphone is not a reason to lose desktop
                // audio. Its meter stays empty, which is what says so.
                eprintln!("could not open audio source {}: {error}", source.name);
                mixer_handle.remove_source(&name);
                levels.peaks.remove(&source.id);
                continue;
            }
        };

        let (volume, volume_handle) = AudioVolume::new(format!("{name}-volume"));
        let _ = volume_handle.set_gain_db(source.gain_db);
        volume_handle.set_muted(source.muted);

        let meter = AppSink::new(format!("{name}-meter"), move |buffer| {
            if let MediaBuffer::Audio(frame) = &buffer {
                peak.store(peak_db(frame).to_bits(), Ordering::Relaxed);
            }
            Ok(())
        });

        builder = builder.add_source(capture, move |source_element, context| {
            let meter_branch = context.branch().to(Box::new(meter))?;
            let mix_branch = context.branch().pipe(volume).to(mixer_input)?;
            let tee = TeeBuilder::new(format!("{name}-tee"), context.clone())
                .branch(meter_branch)
                .branch(mix_branch)
                .build()?;
            context.attach(source_element, 0, tee)?;
            Ok(())
        })?;

        sources.insert(
            source.id,
            OpenAudioSource {
                volume: volume_handle,
                device: source.device.clone(),
            },
        );
    }

    if sources.is_empty() {
        return Err("no audio source could be opened".into());
    }

    // The mixer is a source of its own: it runs on its own clock and emits
    // whether or not anything is listening, which is what a recording
    // attached later needs.
    let pipeline = builder
        .add_source(mixer, |_source, _context| Ok(()))?
        .build();
    pipeline.run()?;
    Ok(Running {
        _pipeline: pipeline,
        sources,
    })
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

/// Owns the audio graph on a thread of its own.
///
/// Separate from `EngineManager` rather than folded into it: building a
/// capture opens the audio subsystem and can block for a moment, which is not
/// something to do on the UI thread — and the video engine's thread exits
/// when its backend cannot start, which is no reason to lose the microphone.
pub struct AudioManager {
    /// `Option` only so `Drop` can close the channel by taking it, which is
    /// what ends the worker's loop.
    commands: Option<Sender<AudioSnapshot>>,
    /// Republished on every rebuild, because the set of sources it covers
    /// changes with them. `None` until the first graph is built.
    levels: Arc<ArcSwapOption<Levels>>,
    worker: Option<JoinHandle<()>>,
}

impl AudioManager {
    pub fn spawn(wake_ui: impl Fn() + Send + 'static) -> std::io::Result<Self> {
        let (commands, command_rx) = mpsc::channel::<AudioSnapshot>();
        let levels = Arc::new(ArcSwapOption::empty());
        let worker = thread::Builder::new().name("audio".to_owned()).spawn({
            let levels = Arc::clone(&levels);
            move || {
                let mut engine = AudioEngine::new();
                // Ends when the sender drops, which is this manager being
                // dropped — and dropping the engine with it stops every
                // capture and the mixer.
                while let Ok(snapshot) = command_rx.recv() {
                    engine.apply(&snapshot);
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
            worker: Some(worker),
        })
    }

    /// Tells the audio graph what the project now holds.
    pub fn apply(&self, snapshot: &AudioSnapshot) {
        if let Some(commands) = &self.commands {
            let _ = commands.send(snapshot.clone());
        }
    }

    /// The most recent peak for one source, or `None` when nothing is
    /// measuring it.
    pub fn peak_db(&self, id: AudioSourceId) -> Option<f32> {
        self.levels.load_full()?.peak_db(id)
    }
}

impl Drop for AudioManager {
    fn drop(&mut self) {
        // The worker's loop ends when this sender goes, so nothing else has
        // to be signalled — but it is joined, because dropping the engine it
        // owns is what stops the captures.
        self.commands = None;
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}
